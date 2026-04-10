import { closeSync, createReadStream, mkdirSync, readFileSync, readSync, writeSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import * as moduleBuiltin from 'node:module';
import { performance as realPerformance } from 'node:perf_hooks';
import path from 'node:path';
import readline from 'node:readline';
import { URL } from 'node:url';

const ACCESS_DENIED_CODE = 'ERR_ACCESS_DENIED';
const ASSET_ROOT_ENV = 'AGENT_OS_NODE_IMPORT_CACHE_ASSET_ROOT';
const PYODIDE_INDEX_URL_ENV = 'AGENT_OS_PYODIDE_INDEX_URL';
const PYODIDE_PACKAGE_BASE_URL_ENV = 'AGENT_OS_PYODIDE_PACKAGE_BASE_URL';
const PYODIDE_PACKAGE_CACHE_DIR_ENV = 'AGENT_OS_PYODIDE_PACKAGE_CACHE_DIR';
const PYODIDE_PACKAGE_CACHE_GUEST_ROOT = '/__agent_os_pyodide_cache';
const PYTHON_CODE_ENV = 'AGENT_OS_PYTHON_CODE';
const PYTHON_FILE_ENV = 'AGENT_OS_PYTHON_FILE';
const PYTHON_PREWARM_ONLY_ENV = 'AGENT_OS_PYTHON_PREWARM_ONLY';
const PYTHON_WARMUP_DEBUG_ENV = 'AGENT_OS_PYTHON_WARMUP_DEBUG';
const PYTHON_WARMUP_METRICS_PREFIX = '__AGENT_OS_PYTHON_WARMUP_METRICS__:';
const PYTHON_PRELOAD_PACKAGES_ENV = 'AGENT_OS_PYTHON_PRELOAD_PACKAGES';
const PYTHON_VFS_RPC_REQUEST_FD_ENV = 'AGENT_OS_PYTHON_VFS_RPC_REQUEST_FD';
const PYTHON_VFS_RPC_RESPONSE_FD_ENV = 'AGENT_OS_PYTHON_VFS_RPC_RESPONSE_FD';
const PYTHON_RUNTIME_ENV_NAMES = ['HOME', 'USER', 'LOGNAME', 'SHELL', 'PWD', 'TMPDIR', 'PATH'];
const ALLOW_PROCESS_BINDINGS = process.env.AGENT_OS_ALLOW_PROCESS_BINDINGS === '1';
const STDIN_FD = 0;
const SUPPORTED_PRELOAD_PACKAGES = ['numpy', 'pandas'];
const SUPPORTED_PRELOAD_PACKAGE_SET = new Set(SUPPORTED_PRELOAD_PACKAGES);
const DENIED_BUILTINS = new Set([
  'child_process',
  'cluster',
  'dgram',
  'diagnostics_channel',
  'dns',
  'http',
  'http2',
  'https',
  'inspector',
  'module',
  'net',
  'tls',
  'trace_events',
  'v8',
  'vm',
  'worker_threads',
]);
const originalFetch =
  typeof globalThis.fetch === 'function'
    ? globalThis.fetch.bind(globalThis)
    : null;
const originalRequire =
  typeof globalThis.require === 'function'
    ? globalThis.require.bind(globalThis)
    : null;
function canCallBridgeSync(bridge) {
  return (
    typeof bridge?.applySyncPromise === 'function' ||
    typeof bridge?.applySync === 'function' ||
    typeof bridge === 'function'
  );
}

function callBridgeSync(bridge, args) {
  if (typeof bridge?.applySyncPromise === 'function') {
    return bridge.applySyncPromise(void 0, args);
  }
  if (typeof bridge?.applySync === 'function') {
    return bridge.applySync(void 0, args);
  }
  if (typeof bridge === 'function') {
    return bridge(...args);
  }
  return undefined;
}

const bridgePythonRpc =
  canCallBridgeSync(globalThis._pythonRpc)
    ? globalThis._pythonRpc
    : null;
const bridgeKernelStdinRead = globalThis._kernelStdinRead ?? null;
const bridgeLoadFileSync =
  canCallBridgeSync(globalThis._loadFileSync)
    ? globalThis._loadFileSync
    : null;
const originalGetBuiltinModule =
  typeof process.getBuiltinModule === 'function'
    ? process.getBuiltinModule.bind(process)
    : null;
const CONTROL_PIPE_FD = parseControlPipeFd(process.env.AGENT_OS_CONTROL_PIPE_FD);
const register = typeof moduleBuiltin?.register === 'function' ? moduleBuiltin.register.bind(moduleBuiltin) : null;

function requiredEnv(name) {
  const value = process.env[name];
  if (value == null) {
    throw new Error(`${name} is required`);
  }
  return value;
}

function parseControlPipeFd(value) {
  if (typeof value !== 'string' || value.trim() === '') {
    return null;
  }

  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

function emitControlMessage(message) {
  if (CONTROL_PIPE_FD == null) {
    return;
  }

  try {
    writeSync(CONTROL_PIPE_FD, `${JSON.stringify(message)}\n`);
  } catch {
    // Ignore control-channel write failures during teardown.
  }
}

function normalizeDirectoryPath(value) {
  return value.endsWith(path.sep) ? value : `${value}${path.sep}`;
}

function pathToFileUrlString(value) {
  const normalizedPath = path.resolve(value);
  return `file://${normalizedPath.startsWith(path.sep) ? normalizedPath : `${path.sep}${normalizedPath}`}`;
}

function fileUrlToPathString(value) {
  const url = value instanceof URL ? value : new URL(String(value));
  if (url.protocol !== 'file:') {
    throw new Error(`Expected file URL, received ${url.protocol}`);
  }
  return decodeURIComponent(url.pathname);
}

function resolveIndexLocation(value) {
  if (/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value)) {
    const normalizedUrl = value.endsWith('/') ? value : `${value}/`;
    if (!normalizedUrl.startsWith('file:')) {
      return {
        indexPath: normalizedUrl,
        indexUrl: normalizedUrl,
      };
    }

    const indexPath = normalizeDirectoryPath(fileUrlToPathString(normalizedUrl));
    return {
      indexPath,
      indexUrl: pathToFileUrlString(indexPath),
    };
  }

  const indexPath = normalizeDirectoryPath(path.resolve(value));
  return {
    indexPath,
    indexUrl: pathToFileUrlString(indexPath),
  };
}

function normalizeBaseUrl(value) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error('package base URL must not be empty');
  }

  if (/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value)) {
    return value.endsWith('/') ? value : `${value}/`;
  }

  return normalizeDirectoryPath(path.resolve(value));
}

function normalizePyodideShellPath(value) {
  if (typeof value !== 'string') {
    throw new Error(`expected shell path to be a string, received ${typeof value}`);
  }

  if (value.startsWith('file://')) {
    return fileUrlToPathString(value);
  }

  if (/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value)) {
    throw new Error(`unsupported Pyodide shell URL ${value}`);
  }

  return value;
}

function installPyodideShellCompat() {
  const originalRead = globalThis.read;
  const originalReadbuffer = globalThis.readbuffer;
  const originalLoad = globalThis.load;
  const originalArguments = globalThis.arguments;
  const originalScriptArgs = globalThis.scriptArgs;
  const originalPrint = globalThis.print;
  const originalPrintErr = globalThis.printErr;

  const shellRead = (target, mode) => {
    const normalized = normalizePyodideShellPath(String(target));
    if (mode === 'binary') {
      return new Uint8Array(readFileSync(normalized));
    }
    return readFileSync(normalized, 'utf8');
  };

  globalThis.read = shellRead;
  globalThis.readbuffer = (target) => new Uint8Array(readFileSync(normalizePyodideShellPath(String(target))));
  globalThis.load = async (target) => {
    const normalized = normalizePyodideShellPath(String(target));
    await import(normalized.startsWith('/') ? normalized : pathToFileUrlString(normalized));
  };
  globalThis.arguments = [];
  globalThis.scriptArgs = [];
  const writeShellStream = (stream, args) => {
    const value = args.join(' ');
    if (value.trim().length === 0) {
      return;
    }
    writeStream(stream, value);
  };

  globalThis.print = (...args) => writeShellStream(process.stdout, args);
  globalThis.printErr = (...args) => writeShellStream(process.stderr, args);

  return () => {
    if (originalRead === undefined) {
      delete globalThis.read;
    } else {
      globalThis.read = originalRead;
    }
    if (originalReadbuffer === undefined) {
      delete globalThis.readbuffer;
    } else {
      globalThis.readbuffer = originalReadbuffer;
    }
    if (originalLoad === undefined) {
      delete globalThis.load;
    } else {
      globalThis.load = originalLoad;
    }
    if (originalArguments === undefined) {
      delete globalThis.arguments;
    } else {
      globalThis.arguments = originalArguments;
    }
    if (originalScriptArgs === undefined) {
      delete globalThis.scriptArgs;
    } else {
      globalThis.scriptArgs = originalScriptArgs;
    }
    if (originalPrint === undefined) {
      delete globalThis.print;
    } else {
      globalThis.print = originalPrint;
    }
    if (originalPrintErr === undefined) {
      delete globalThis.printErr;
    } else {
      globalThis.printErr = originalPrintErr;
    }
  };
}

function resolvePyodideResource(indexPath, indexUrl, resourceName) {
  if (typeof indexPath === 'string' && path.isAbsolute(indexPath)) {
    const resourcePath = path.join(indexPath, resourceName);
    return {
      path: resourcePath,
      url: resourcePath,
    };
  }

  const resourceUrl = new URL(resourceName, indexUrl).href;
  return {
    path: resourceUrl,
    url: resourceUrl,
  };
}

function writeStream(stream, message) {
  if (message == null) {
    return;
  }

  const value = typeof message === 'string' ? message : String(message);
  stream.write(value.endsWith('\n') ? value : `${value}\n`);
}

function writePyodideStdout(message) {
  if (message == null) {
    return;
  }

  const value = typeof message === 'string' ? message : String(message);
  const trimmed = value.trim();
  if (trimmed.length === 0) {
    return;
  }
  if (
    trimmed.startsWith('Loading ') ||
    trimmed.startsWith('Loaded ') ||
    trimmed.startsWith("Didn't find package ") ||
    (trimmed.startsWith('Package ') && trimmed.includes(' loaded from '))
  ) {
    return;
  }

  writeStream(process.stdout, value);
}

function resolvePyodidePackageCacheDir() {
  if (bridgePythonRpc) {
    return PYODIDE_PACKAGE_CACHE_GUEST_ROOT;
  }

  const configured = process.env[PYODIDE_PACKAGE_CACHE_DIR_ENV];
  if (typeof configured === 'string' && configured.trim() !== '') {
    return path.resolve(configured);
  }

  const assetRoot = process.env[ASSET_ROOT_ENV];
  if (typeof assetRoot === 'string' && assetRoot.trim() !== '') {
    return path.resolve(assetRoot, '..', 'pyodide-package-cache');
  }

  return path.resolve(process.cwd(), '.agent-os-pyodide-cache');
}

function emitWarmupStage(stage) {
  if (process.env[PYTHON_WARMUP_DEBUG_ENV] !== '1') {
    return;
  }

  writeStream(process.stderr, `__AGENT_OS_PYTHON_WARMUP_STAGE__:${stage}`);
}

function emitPythonDebug(channel, message) {
  if (process.env[PYTHON_WARMUP_DEBUG_ENV] !== '1') {
    return;
  }

  writeStream(process.stderr, `__AGENT_OS_PYTHON_${channel}__:${message}`);
}

function formatError(error) {
  if (error instanceof Error) {
    return error.stack || error.message || String(error);
  }

  return String(error);
}

function wrapPythonStartupError(step, context, error) {
  const details = Object.entries(context)
    .filter(([, value]) => value != null && value !== '')
    .map(([key, value]) => `${key}=${value}`)
    .join(', ');
  const suffix = details.length > 0 ? ` (${details})` : '';
  return new Error(`Python runtime ${step} failed${suffix}: ${formatError(error)}`);
}

function normalizeFetchHeaders(headers) {
  if (headers == null) {
    return {};
  }

  if (headers instanceof Headers) {
    return Object.fromEntries(headers.entries());
  }

  if (Array.isArray(headers)) {
    return Object.fromEntries(headers);
  }

  return Object.fromEntries(Object.entries(headers).map(([key, value]) => [key, String(value)]));
}

async function normalizeFetchBody(body) {
  if (body == null) {
    return null;
  }

  if (typeof body === 'string') {
    return Buffer.from(body).toString('base64');
  }

  if (ArrayBuffer.isView(body)) {
    return Buffer.from(body.buffer, body.byteOffset, body.byteLength).toString('base64');
  }

  if (body instanceof ArrayBuffer) {
    return Buffer.from(body).toString('base64');
  }

  if (typeof Blob !== 'undefined' && body instanceof Blob) {
    return Buffer.from(await body.arrayBuffer()).toString('base64');
  }

  throw new Error('unsupported fetch body type for Agent OS Python package loading');
}

function emitPythonStartupMetrics({
  prewarmOnly,
  startupMs,
  loadPyodideMs,
  packageLoadMs,
  packageCount,
  source,
}) {
  if (process.env[PYTHON_WARMUP_DEBUG_ENV] !== '1') {
    return;
  }

  writeStream(
    process.stderr,
    `${PYTHON_WARMUP_METRICS_PREFIX}${JSON.stringify({
      phase: 'startup',
      prewarmOnly,
      startupMs,
      loadPyodideMs,
      packageLoadMs,
      packageCount,
      source,
    })}`,
  );
}

function parsePreloadPackages(value) {
  if (value == null || value.trim() === '') {
    return [];
  }

  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch (error) {
    throw new Error(
      `${PYTHON_PRELOAD_PACKAGES_ENV} must be a JSON array of package names: ${formatError(error)}`,
    );
  }

  if (!Array.isArray(parsed)) {
    throw new Error(`${PYTHON_PRELOAD_PACKAGES_ENV} must be a JSON array of package names`);
  }

  const packages = [];
  const seen = new Set();

  for (const entry of parsed) {
    if (typeof entry !== 'string') {
      throw new Error(`${PYTHON_PRELOAD_PACKAGES_ENV} entries must be strings`);
    }

    const name = entry.trim();
    if (name.length === 0) {
      throw new Error(`${PYTHON_PRELOAD_PACKAGES_ENV} entries must not be empty`);
    }

    if (!SUPPORTED_PRELOAD_PACKAGE_SET.has(name)) {
      throw new Error(
        `Unsupported bundled Python package "${name}". Available packages: ${SUPPORTED_PRELOAD_PACKAGES.join(', ')}`,
      );
    }

    if (!seen.has(name)) {
      seen.add(name);
      packages.push(name);
    }
  }

  return packages;
}

function parseOptionalFd(name) {
  const value = process.env[name];
  if (value == null || value.trim() === '') {
    return null;
  }

  const fd = Number.parseInt(value, 10);
  if (!Number.isInteger(fd) || fd < 0) {
    throw new Error(`${name} must be a non-negative integer file descriptor`);
  }

  return fd;
}

function normalizeWriteContent(content) {
  if (typeof content === 'string') {
    return content;
  }
  if (ArrayBuffer.isView(content)) {
    return Buffer.from(content.buffer, content.byteOffset, content.byteLength).toString('base64');
  }
  if (content instanceof ArrayBuffer) {
    return Buffer.from(content).toString('base64');
  }
  throw new Error('fsWrite requires a base64 string or Uint8Array');
}

function rejectPendingRpcRequests(pending, error) {
  for (const { reject } of pending.values()) {
    reject(error);
  }
  pending.clear();
}

function normalizePythonBridgeError(error) {
  const normalized = error instanceof Error ? error : new Error(String(error));
  const message = normalized.message || String(error);
  const separatorIndex = message.indexOf(': ');
  if (separatorIndex > 0) {
    const code = message.slice(0, separatorIndex);
    if (/^(?:ERR_[A-Z0-9_]+|E[A-Z0-9_]+)$/.test(code)) {
      normalized.code = code;
      normalized.message = message.slice(separatorIndex + 2);
    }
  }
  if (typeof normalized.code !== 'string') {
    normalized.code = 'ERR_AGENT_OS_PYTHON_VFS_RPC';
  }
  return normalized;
}

function createPythonBridgeRpcBridge() {
  if (!bridgePythonRpc) {
    return null;
  }

  function requestSync(method, payload = {}) {
    try {
      return callBridgeSync(bridgePythonRpc, [{
        method,
        ...payload,
      }]) ?? {};
    } catch (error) {
      throw normalizePythonBridgeError(error);
    }
  }

  return {
    fsReadSync(path) {
      const result = requestSync('fsRead', { path });
      return result.contentBase64 ?? '';
    },
    async fsRead(path) {
      return this.fsReadSync(path);
    },
    fsWriteSync(path, content) {
      requestSync('fsWrite', {
        path,
        contentBase64: normalizeWriteContent(content),
      });
    },
    async fsWrite(path, content) {
      this.fsWriteSync(path, content);
    },
    fsStatSync(path) {
      const result = requestSync('fsStat', { path });
      return result.stat ?? null;
    },
    async fsStat(path) {
      return this.fsStatSync(path);
    },
    fsReaddirSync(path) {
      const result = requestSync('fsReaddir', { path });
      return result.entries ?? [];
    },
    async fsReaddir(path) {
      return this.fsReaddirSync(path);
    },
    fsMkdirSync(path, options = {}) {
      requestSync('fsMkdir', {
        path,
        recursive: options?.recursive === true,
      });
    },
    async fsMkdir(path, options = {}) {
      this.fsMkdirSync(path, options);
    },
    httpRequestSync(url, method = 'GET', headersJson = '{}', bodyBase64 = null) {
      let headers;
      try {
        headers = JSON.parse(headersJson);
      } catch (error) {
        throw new Error(`invalid Python httpRequest headers JSON: ${formatError(error)}`);
      }
      return JSON.stringify(requestSync('httpRequest', {
        url,
        httpMethod: method,
        headers,
        bodyBase64,
      }));
    },
    dnsLookupSync(hostname, family = null) {
      return JSON.stringify(requestSync('dnsLookup', { hostname, family }));
    },
    subprocessRunSync(
      command,
      argsJson = '[]',
      cwd = null,
      envJson = '{}',
      shell = false,
      maxBuffer = null,
    ) {
      let args;
      let env;
      try {
        args = JSON.parse(argsJson);
        env = JSON.parse(envJson);
      } catch (error) {
        throw new Error(`invalid Python subprocessRun payload JSON: ${formatError(error)}`);
      }
      return JSON.stringify(requestSync('subprocessRun', {
        command,
        args,
        cwd,
        env,
        shell,
        maxBuffer,
      }));
    },
    dispose() {},
  };
}

function createPythonFdRpcBridge() {
  const requestFd = parseOptionalFd(PYTHON_VFS_RPC_REQUEST_FD_ENV);
  const responseFd = parseOptionalFd(PYTHON_VFS_RPC_RESPONSE_FD_ENV);

  if (requestFd == null && responseFd == null) {
    return null;
  }

  if (requestFd == null || responseFd == null) {
    throw new Error(
      `both ${PYTHON_VFS_RPC_REQUEST_FD_ENV} and ${PYTHON_VFS_RPC_RESPONSE_FD_ENV} are required`,
    );
  }

  let nextRequestId = 1;
  const queuedResponses = new Map();
  let responseBuffer = '';

  function readResponseLineSync() {
    while (true) {
      const newlineIndex = responseBuffer.indexOf('\n');
      if (newlineIndex >= 0) {
        const line = responseBuffer.slice(0, newlineIndex);
        responseBuffer = responseBuffer.slice(newlineIndex + 1);
        return line;
      }

      const chunk = Buffer.alloc(4096);
      const bytesRead = readSync(responseFd, chunk, 0, chunk.length, null);
      if (bytesRead === 0) {
        throw new Error('Agent OS Python VFS RPC response channel closed unexpectedly');
      }
      responseBuffer += chunk.subarray(0, bytesRead).toString('utf8');
    }
  }

  function parseResponseLine(line) {
    try {
      return JSON.parse(line);
    } catch (error) {
      throw new Error(`invalid Agent OS Python VFS RPC response: ${formatError(error)}`);
    }
  }

  function waitForResponseSync(id) {
    const queued = queuedResponses.get(id);
    if (queued) {
      queuedResponses.delete(id);
      return queued;
    }

    while (true) {
      const line = readResponseLineSync();
      if (line.trim() === '') {
        continue;
      }

      const message = parseResponseLine(line);
      if (message?.id === id) {
        return message;
      }
      queuedResponses.set(message?.id, message);
    }
  }

  function requestSync(method, payload = {}) {
    const id = nextRequestId++;
    writeSync(
      requestFd,
      `${JSON.stringify({
        id,
        method,
        ...payload,
      })}\n`,
    );

    const message = waitForResponseSync(id);
    if (message?.ok) {
      return message.result ?? {};
    }

    const error = new Error(message?.error?.message || `Agent OS Python VFS RPC request ${id} failed`);
    error.code = message?.error?.code || 'ERR_AGENT_OS_PYTHON_VFS_RPC';
    throw error;
  }

  function request(method, payload = {}) {
    return Promise.resolve().then(() => requestSync(method, payload));
  }

  return {
    fsReadSync(path) {
      const result = requestSync('fsRead', { path });
      return result.contentBase64 ?? '';
    },
    async fsRead(path) {
      return this.fsReadSync(path);
    },
    fsWriteSync(path, content) {
      requestSync('fsWrite', {
        path,
        contentBase64: normalizeWriteContent(content),
      });
    },
    async fsWrite(path, content) {
      this.fsWriteSync(path, content);
    },
    fsStatSync(path) {
      const result = requestSync('fsStat', { path });
      return result.stat ?? null;
    },
    async fsStat(path) {
      return this.fsStatSync(path);
    },
    fsReaddirSync(path) {
      const result = requestSync('fsReaddir', { path });
      return result.entries ?? [];
    },
    async fsReaddir(path) {
      return this.fsReaddirSync(path);
    },
    fsMkdirSync(path, options = {}) {
      requestSync('fsMkdir', {
        path,
        recursive: options?.recursive === true,
      });
    },
    async fsMkdir(path, options = {}) {
      this.fsMkdirSync(path, options);
    },
    httpRequestSync(url, method = 'GET', headersJson = '{}', bodyBase64 = null) {
      let headers;
      try {
        headers = JSON.parse(headersJson);
      } catch (error) {
        throw new Error(`invalid Python httpRequest headers JSON: ${formatError(error)}`);
      }
      return JSON.stringify(requestSync('httpRequest', {
        url,
        httpMethod: method,
        headers,
        bodyBase64,
      }));
    },
    dnsLookupSync(hostname, family = null) {
      return JSON.stringify(requestSync('dnsLookup', { hostname, family }));
    },
    subprocessRunSync(
      command,
      argsJson = '[]',
      cwd = null,
      envJson = '{}',
      shell = false,
      maxBuffer = null,
    ) {
      let args;
      let env;
      try {
        args = JSON.parse(argsJson);
        env = JSON.parse(envJson);
      } catch (error) {
        throw new Error(`invalid Python subprocessRun payload JSON: ${formatError(error)}`);
      }
      return JSON.stringify(requestSync('subprocessRun', {
        command,
        args,
        cwd,
        env,
        shell,
        maxBuffer,
      }));
    },
    dispose() {
      try {
        closeSync(requestFd);
      } catch {
        // Ignore repeated-close shutdown races.
      }
      try {
        closeSync(responseFd);
      } catch {
        // Ignore repeated-close shutdown races.
      }
    },
  };
}

function accessDenied(subject) {
  const error = new Error(`${subject} is not available in the Agent OS guest Python runtime`);
  error.code = ACCESS_DENIED_CODE;
  return error;
}

const PYTHON_GUEST_IMPORT_BLOCKLIST_SOURCE = String.raw`
import builtins as _agent_os_builtins
import sys as _agent_os_sys
import types as _agent_os_types

try:
    import agent_os_internal_js as _agent_os_safe_js
    import agent_os_internal_pyodide_js as _agent_os_safe_pyodide_js
    import agent_os_internal_pyodide_js_api as _agent_os_safe_pyodide_js_api
except Exception:
    _agent_os_safe_js = None
    _agent_os_safe_pyodide_js = None
    _agent_os_safe_pyodide_js_api = None

def _agent_os_raise_access_denied(module_name):
    raise RuntimeError(f"{module_name} is not available in the Agent OS guest Python runtime")

class _AgentOsBlockedModule(_agent_os_types.ModuleType):
    def __init__(self, name):
        super().__init__(name)
        self.__dict__['__all__'] = ()

    def __getattr__(self, _name):
        _agent_os_raise_access_denied(self.__name__)

    def __dir__(self):
        return []

_agent_os_blocked_modules = {
    _agent_os_module_name: _AgentOsBlockedModule(_agent_os_module_name)
    for _agent_os_module_name in ('js', 'pyodide_js')
}

_agent_os_safe_modules = {
    "js": _agent_os_safe_js,
    "pyodide_js": _agent_os_safe_pyodide_js,
    "pyodide_js._api": _agent_os_safe_pyodide_js_api,
}

_agent_os_original_import = _agent_os_builtins.__import__

def _agent_os_allow_internal_js(globals):
    module_name = str((globals or {}).get("__name__", ""))
    return module_name.startswith("micropip") or module_name.startswith("pyodide.http")

def _agent_os_import(name, globals=None, locals=None, fromlist=(), level=0):
    if name in _agent_os_safe_modules and _agent_os_safe_modules[name] is not None and _agent_os_allow_internal_js(globals):
        return _agent_os_safe_modules[name]
    if name in _agent_os_blocked_modules:
        return _agent_os_blocked_modules[name]
    return _agent_os_original_import(name, globals, locals, fromlist, level)

_agent_os_builtins.__import__ = _agent_os_import
_agent_os_sys.modules.update(_agent_os_blocked_modules)
`;

const PYTHON_KERNEL_RPC_SHIMS_SOURCE = String.raw`
import base64 as _agent_os_base64
import json as _agent_os_json
import socket as _agent_os_socket
import subprocess as _agent_os_subprocess
import sys as _agent_os_sys
import types as _agent_os_types
import urllib.error as _agent_os_urllib_error
import urllib.request as _agent_os_urllib_request
from email.message import Message as _AgentOsMessage
from js import __agentOsPythonVfsRpc as _agent_os_rpc

def _agent_os_raise_from_error(error):
    if not isinstance(error, dict):
        raise RuntimeError(str(error))
    message = str(error.get("message", "Agent OS Python bridge request failed"))
    if "EACCES:" in message:
        raise PermissionError(message)
    if "command not found" in message:
        raise FileNotFoundError(message)
    raise OSError(message)

def _agent_os_normalize_family(family):
    if family in (None, 0):
        return None
    if family == _agent_os_socket.AF_INET:
        return 4
    if family == _agent_os_socket.AF_INET6:
        return 6
    return None

def _agent_os_dns_lookup(hostname, family=None):
    try:
        result = _agent_os_json.loads(
            _agent_os_rpc.dnsLookupSync(hostname, _agent_os_normalize_family(family))
        )
    except Exception as error:
        _agent_os_raise_from_error({"message": str(error)})
    addresses = result.get("addresses") or []
    if not addresses:
        raise OSError(f"Agent OS DNS lookup returned no addresses for {hostname}")
    return addresses

class _AgentOsHttpResponse:
    def __init__(self, payload):
        self.status = int(payload.get("status", 0))
        self.reason = str(payload.get("reason", ""))
        self.url = str(payload.get("url", ""))
        self._body = _agent_os_base64.b64decode(payload.get("bodyBase64", "") or "")
        headers = payload.get("headers") or {}
        self.headers = _AgentOsMessage()
        for name, values in headers.items():
          for value in values:
            self.headers.add_header(str(name), str(value))

    def read(self, amt=-1):
        if amt is None or amt < 0:
            return self._body
        return self._body[:amt]

    def getcode(self):
        return self.status

    def info(self):
        return self.headers

    def close(self):
        return None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()
        return False

class _AgentOsPyfetchResponse:
    def __init__(self, payload):
        self.status = int(payload.get("status", 0))
        self.status_text = str(payload.get("reason", ""))
        self.url = str(payload.get("url", ""))
        self.headers = {str(name): ", ".join(values) for name, values in (payload.get("headers") or {}).items()}
        self._body = _agent_os_base64.b64decode(payload.get("bodyBase64", "") or "")

    async def bytes(self):
        return self._body

    async def string(self):
        return self._body.decode("utf-8", errors="replace")

    def raise_for_status(self):
        if self.status >= 400:
            raise RuntimeError(f"{self.status} {self.status_text}")

def _agent_os_extract_request_parts(url_or_request, data=None):
    if isinstance(url_or_request, _agent_os_urllib_request.Request):
        request = url_or_request
        url = request.full_url
        method = request.get_method()
        headers = dict(request.header_items())
        payload = request.data if data is None else data
    else:
        url = str(url_or_request)
        method = "POST" if data is not None else "GET"
        headers = {}
        payload = data
    body_base64 = None
    if payload is not None:
        if isinstance(payload, str):
            payload = payload.encode("utf-8")
        body_base64 = _agent_os_base64.b64encode(payload).decode("ascii")
    return url, method, headers, body_base64

def _agent_os_http_request(url_or_request, data=None):
    url, method, headers, body_base64 = _agent_os_extract_request_parts(url_or_request, data)
    try:
        payload = _agent_os_json.loads(
            _agent_os_rpc.httpRequestSync(url, method, _agent_os_json.dumps(headers), body_base64)
        )
    except Exception as error:
        _agent_os_raise_from_error({"message": str(error)})
    response = _AgentOsHttpResponse(payload)
    if response.status >= 400:
        raise _agent_os_urllib_error.HTTPError(
            url,
            response.status,
            response.reason,
            response.headers,
            response,
        )
    return response

async def _agent_os_pyfetch(url, **kwargs):
    headers = dict(kwargs.get("headers") or {})
    method = str(kwargs.get("method", "GET")).upper()
    body = kwargs.get("body")
    if body is not None and isinstance(body, str):
        body = body.encode("utf-8")
    body_base64 = None if body is None else _agent_os_base64.b64encode(body).decode("ascii")
    try:
        payload = _agent_os_json.loads(
            _agent_os_rpc.httpRequestSync(
                str(url),
                method,
                _agent_os_json.dumps(headers),
                body_base64,
            )
        )
    except Exception as error:
        _agent_os_raise_from_error({"message": str(error)})
    return _AgentOsPyfetchResponse(payload)

def _agent_os_urlopen(url, data=None, timeout=None, *args, **kwargs):
    del timeout, args, kwargs
    return _agent_os_http_request(url, data=data)

_agent_os_urllib_request.urlopen = _agent_os_urlopen

try:
    import pyodide.http as _agent_os_pyodide_http
except ModuleNotFoundError:
    _agent_os_pyodide_http = None
else:
    _agent_os_pyodide_http.pyfetch = _agent_os_pyfetch

_agent_os_original_getaddrinfo = _agent_os_socket.getaddrinfo

def _agent_os_getaddrinfo(host, port, family=0, type=0, proto=0, flags=0):
    if host in (None, "", "0.0.0.0", "::"):
        return _agent_os_original_getaddrinfo(host, port, family, type, proto, flags)
    addresses = _agent_os_dns_lookup(host, family)
    socktype = type or _agent_os_socket.SOCK_STREAM
    protocol = proto or 0
    normalized_family = family or _agent_os_socket.AF_INET
    results = []
    for address in addresses:
        entry_family = _agent_os_socket.AF_INET6 if ":" in address else _agent_os_socket.AF_INET
        if family not in (0, entry_family):
            continue
        if entry_family == _agent_os_socket.AF_INET6:
            sockaddr = (address, port, 0, 0)
        else:
            sockaddr = (address, port)
        results.append((entry_family, socktype, protocol, "", sockaddr))
    if not results:
        raise OSError(f"Agent OS DNS lookup returned no matching addresses for {host}")
    return results

def _agent_os_gethostbyname(host):
    return _agent_os_dns_lookup(host, _agent_os_socket.AF_INET)[0]

_agent_os_socket.getaddrinfo = _agent_os_getaddrinfo
_agent_os_socket.gethostbyname = _agent_os_gethostbyname

class _AgentOsRequestsResponse:
    def __init__(self, payload):
        self.status_code = int(payload.get("status", 0))
        self.reason = str(payload.get("reason", ""))
        self.url = str(payload.get("url", ""))
        self.headers = {str(name): ", ".join(values) for name, values in (payload.get("headers") or {}).items()}
        self.content = _agent_os_base64.b64decode(payload.get("bodyBase64", "") or "")
        self.encoding = "utf-8"
        self.ok = self.status_code < 400

    @property
    def text(self):
        return self.content.decode(self.encoding, errors="replace")

    def json(self):
        return _agent_os_json.loads(self.text)

    def raise_for_status(self):
        if self.status_code >= 400:
            raise RuntimeError(f"{self.status_code} {self.reason}")

class _AgentOsRequestsSession:
    def request(self, method, url, **kwargs):
        headers = dict(kwargs.get("headers") or {})
        data = kwargs.get("data")
        if data is not None and isinstance(data, str):
            data = data.encode("utf-8")
        body_base64 = None if data is None else _agent_os_base64.b64encode(data).decode("ascii")
        try:
            payload = _agent_os_json.loads(
                _agent_os_rpc.httpRequestSync(
                    str(url),
                    str(method).upper(),
                    _agent_os_json.dumps(headers),
                    body_base64,
                )
            )
        except Exception as error:
            _agent_os_raise_from_error({"message": str(error)})
        return _AgentOsRequestsResponse(payload)

    def get(self, url, **kwargs):
        return self.request("GET", url, **kwargs)

def _agent_os_install_requests_module():
    module = _agent_os_types.ModuleType("requests")
    session = _AgentOsRequestsSession
    module.Session = session
    module.Response = _AgentOsRequestsResponse
    module.request = lambda method, url, **kwargs: session().request(method, url, **kwargs)
    module.get = lambda url, **kwargs: session().get(url, **kwargs)
    module.exceptions = _agent_os_types.SimpleNamespace(RequestException=RuntimeError)
    _agent_os_sys.modules["requests"] = module

try:
    import requests as _agent_os_requests
except ModuleNotFoundError:
    _agent_os_install_requests_module()
else:
    _agent_os_requests.Session = _AgentOsRequestsSession
    _agent_os_requests.Response = _AgentOsRequestsResponse
    _agent_os_requests.request = lambda method, url, **kwargs: _AgentOsRequestsSession().request(method, url, **kwargs)
    _agent_os_requests.get = lambda url, **kwargs: _AgentOsRequestsSession().get(url, **kwargs)

class _AgentOsCompletedProcess:
    def __init__(self, args, returncode, stdout, stderr):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def _agent_os_subprocess_run(args, *, capture_output=False, check=False, cwd=None, env=None, input=None, shell=False, text=False, encoding="utf-8", errors="strict", stdout=None, stderr=None, timeout=None, **kwargs):
    del kwargs, stdout, stderr, timeout
    if isinstance(args, (str, bytes)):
        command = args.decode("utf-8") if isinstance(args, bytes) else args
        argv = []
    else:
        values = list(args)
        if not values:
            raise ValueError("subprocess.run args must not be empty")
        command = str(values[0])
        argv = [str(value) for value in values[1:]]
    merged_env = dict(env or {})
    resolved_cwd = cwd if cwd is not None else _agent_os_os.environ.get("PWD")
    if input is not None:
        raise NotImplementedError("subprocess.run input is not supported in the Agent OS Python runtime")
    try:
        payload = _agent_os_json.loads(
            _agent_os_rpc.subprocessRunSync(
                command,
                _agent_os_json.dumps(argv),
                resolved_cwd,
                _agent_os_json.dumps(merged_env),
                bool(shell),
            )
        )
    except Exception as error:
        _agent_os_raise_from_error({"message": str(error)})
    stdout_bytes = payload.get("stdout", "").encode("utf-8")
    stderr_bytes = payload.get("stderr", "").encode("utf-8")
    if text or encoding is not None:
        stdout_value = stdout_bytes.decode(encoding or "utf-8", errors=errors)
        stderr_value = stderr_bytes.decode(encoding or "utf-8", errors=errors)
    else:
        stdout_value = stdout_bytes
        stderr_value = stderr_bytes
    result = _AgentOsCompletedProcess(
        args,
        int(payload.get("exitCode", 1)),
        stdout_value if capture_output else None,
        stderr_value if capture_output else None,
    )
    if check and result.returncode != 0:
        raise _agent_os_subprocess.CalledProcessError(
            result.returncode,
            args,
            output=result.stdout,
            stderr=result.stderr,
        )
    return result

_agent_os_subprocess.run = _agent_os_subprocess_run
`;

function hardenProperty(target, key, value) {
  try {
    Object.defineProperty(target, key, {
      value,
      writable: false,
      configurable: false,
    });
  } catch (error) {
    try {
      target[key] = value;
      if (target[key] === value) {
        return;
      }
    } catch {
      // Fall through to the original hardening error.
    }
    throw new Error(`Failed to harden property ${String(key)}`, { cause: error });
  }
}

function normalizeBuiltin(specifier) {
  if (typeof specifier !== 'string') {
    return null;
  }

  return specifier.startsWith('node:') ? specifier.slice('node:'.length) : specifier;
}

function installPythonGuestImportBlocklist(pyodide) {
  if (typeof pyodide?.runPython !== 'function') {
    return;
  }

  pyodide.runPython(PYTHON_GUEST_IMPORT_BLOCKLIST_SOURCE);
}

function buildPythonRuntimeEnv() {
  const runtimeEnv = {};
  for (const name of PYTHON_RUNTIME_ENV_NAMES) {
    if (typeof process.env[name] === 'string') {
      runtimeEnv[name] = process.env[name];
    }
  }
  return runtimeEnv;
}

function installPythonRuntimeEnv(pyodide) {
  if (typeof pyodide?.runPython !== 'function') {
    return;
  }

  const runtimeEnv = buildPythonRuntimeEnv();

  pyodide.runPython(`
import json as _agent_os_json
import os as _agent_os_os

for _agent_os_key, _agent_os_value in _agent_os_json.loads(${JSON.stringify(JSON.stringify(runtimeEnv))}).items():
    _agent_os_os.environ[_agent_os_key] = _agent_os_value
`);
}

function installPythonKernelRpcShims(pyodide) {
  if (typeof pyodide?.runPython !== 'function' || !globalThis.__agentOsPythonVfsRpc) {
    return;
  }

  pyodide.runPython(PYTHON_KERNEL_RPC_SHIMS_SOURCE);
}

function installPythonMicropipCompat(pyodide) {
  if (typeof pyodide?.registerJsModule !== 'function') {
    return;
  }

  const abortSignalAny = (signals) => {
    const values = Array.from(signals ?? []);
    if (typeof AbortSignal?.any === 'function') {
      return AbortSignal.any(values);
    }

    const controller = new AbortController();
    for (const signal of values) {
      if (!signal) {
        continue;
      }
      if (signal.aborted) {
        controller.abort(signal.reason);
        return controller.signal;
      }
      signal.addEventListener?.(
        'abort',
        () => {
          if (!controller.signal.aborted) {
            controller.abort(signal.reason);
          }
        },
        { once: true },
      );
    }
    return controller.signal;
  };

  pyodide.registerJsModule('agent_os_internal_js', {
    AbortController,
    AbortSignal,
    Object,
    Request,
    fetch: globalThis.fetch,
  });
  const pyodideApiCompat = {
    abortSignalAny,
    install: pyodide?._api?.install,
    loadBinaryFile: pyodide?._api?.loadBinaryFile,
    lockfile_info: pyodide?._api?.lockfile_info,
    lockfile_packages: pyodide?._api?.lockfile_packages,
  };
  pyodide.registerJsModule('agent_os_internal_pyodide_js', {
    loadedPackages: pyodide.loadedPackages,
    loadPackage: pyodide.loadPackage?.bind(pyodide),
    lockfileBaseUrl: pyodide?._api?.config?.packageBaseUrl ?? '',
    _api: pyodideApiCompat,
  });
  pyodide.registerJsModule('agent_os_internal_pyodide_js_api', pyodideApiCompat);
}

function installPythonGuestPreloadHardening(bridge = null) {
  if (originalRequire) {
    hardenProperty(globalThis, 'require', () => {
      throw accessDenied('require');
    });
  }

  if (originalFetch) {
    const restrictedFetch = async (resource, init = {}) => {
      const request = typeof Request !== 'undefined' && resource instanceof Request ? resource : null;
      const candidate =
        typeof resource === 'string'
          ? resource
          : resource instanceof URL
            ? resource.href
            : request?.url;

      let url;
      try {
        url = new URL(String(candidate ?? ''));
      } catch {
        throw accessDenied('network access');
      }

      if (url.protocol === 'data:' || url.protocol === 'file:') {
        emitPythonDebug('HTTP_DEBUG', `fetch:passthrough:${url.href}`);
        return originalFetch(resource, init);
      }

      if ((url.protocol === 'http:' || url.protocol === 'https:') && bridge) {
        const method = (init.method ?? request?.method ?? 'GET').toUpperCase();
        const headers = normalizeFetchHeaders(init.headers ?? request?.headers);
        const bodyBase64 = await normalizeFetchBody(init.body ?? null);
        emitPythonDebug('HTTP_DEBUG', `fetch:start:${method}:${url.href}`);
        const payload = JSON.parse(
          bridge.httpRequestSync(url.href, method, JSON.stringify(headers), bodyBase64),
        );
        emitPythonDebug(
          'HTTP_DEBUG',
          `fetch:ok:${payload.status ?? 0}:${url.href}`,
        );
        const responseBody = Buffer.from(payload.bodyBase64 ?? '', 'base64');
        return new Response(responseBody, {
          status: payload.status,
          statusText: payload.reason,
          headers: payload.headers ?? {},
        });
      }

      if (url.protocol !== 'data:' && url.protocol !== 'file:') {
        throw accessDenied(`network access to ${url.protocol}`);
      }
      return originalFetch(resource, init);
    };

    try {
      hardenProperty(globalThis, 'fetch', restrictedFetch);
    } catch {
      // The shared JS runtime may have already sealed fetch with its own restrictions.
    }
  }
}

function installPythonGuestProcessHardening() {
  if (!ALLOW_PROCESS_BINDINGS) {
    hardenProperty(process, 'binding', () => {
      throw accessDenied('process.binding');
    });
    hardenProperty(process, '_linkedBinding', () => {
      throw accessDenied('process._linkedBinding');
    });
    hardenProperty(process, 'dlopen', () => {
      throw accessDenied('process.dlopen');
    });
  }

  if (originalGetBuiltinModule) {
    hardenProperty(process, 'getBuiltinModule', (specifier) => {
      const normalized = normalizeBuiltin(specifier);
      if (normalized && DENIED_BUILTINS.has(normalized)) {
        throw accessDenied(`node:${normalized}`);
      }
      return originalGetBuiltinModule(specifier);
    });
  }
}

function installPythonGuestLoaderHooks() {
  const assetRoot = process.env[ASSET_ROOT_ENV];
  if (!assetRoot || !register) {
    return;
  }

  register(new URL('./loader.mjs', import.meta.url), import.meta.url);
}

function installPythonVfsRpcBridge() {
  const bridge = createPythonBridgeRpcBridge() ?? createPythonFdRpcBridge();
  if (!bridge) {
    return null;
  }

  hardenProperty(globalThis, '__agentOsPythonVfsRpc', bridge);
  return bridge;
}

function installPythonWorkspaceFs(pyodide, bridge) {
  if (!bridge) {
    return;
  }

  const { FS, ERRNO_CODES } = pyodide;
  if (!FS?.mount || !FS?.filesystems?.MEMFS || !ERRNO_CODES) {
    return;
  }

  const MEMFS = FS.filesystems.MEMFS;
  const memfsDirNodeOps = MEMFS.ops_table.dir.node;
  const memfsDirStreamOps = MEMFS.ops_table.dir.stream;
  const memfsFileNodeOps = MEMFS.ops_table.file.node;
  const memfsFileStreamOps = MEMFS.ops_table.file.stream;
  const workspaceDirStreamOps = memfsDirStreamOps;

  function joinGuestPath(parentPath, name) {
    return parentPath === '/' ? `/${name}` : `${parentPath}/${name}`;
  }

  function nodeGuestPath(node) {
    return node.agentOsGuestPath || node.mount?.mountpoint || '/workspace';
  }

  function createFsError(error) {
    if (error instanceof FS.ErrnoError) {
      return error;
    }

    const diagnostic = `${error?.code || ''} ${error?.message || ''} ${error?.stack || ''}`;
    const message = diagnostic.toLowerCase();
    let errno = ERRNO_CODES.EIO;
    if (/permission denied|access denied|denied/.test(message)) {
      errno = ERRNO_CODES.EACCES;
    } else if (/read-only|erofs/.test(message)) {
      errno = ERRNO_CODES.EROFS;
    } else if (/not a directory|enotdir/.test(message)) {
      errno = ERRNO_CODES.ENOTDIR;
    } else if (/is a directory|eisdir/.test(message)) {
      errno = ERRNO_CODES.EISDIR;
    } else if (/exists|already exists|eexist/.test(message)) {
      errno = ERRNO_CODES.EEXIST;
    } else if (/not found|no such file|enoent/.test(message)) {
      errno = ERRNO_CODES.ENOENT;
    }

    return new FS.ErrnoError(errno);
  }

  function withFsErrors(operation) {
    try {
      return operation();
    } catch (error) {
      throw createFsError(error);
    }
  }

  function updateNodeFromRemoteStat(node, stat) {
    if (!stat) {
      throw new FS.ErrnoError(ERRNO_CODES.ENOENT);
    }

    node.mode = stat.mode;
    node.timestamp = Date.now();
    if (FS.isFile(stat.mode) && !node.agentOsDirty) {
      node.agentOsRemoteSize = stat.size;
    }
  }

  function createWorkspaceNode(parent, name, mode, dev, guestPath) {
    const node = MEMFS.createNode(parent, name, mode, dev);
    node.agentOsGuestPath = guestPath;
    node.agentOsDirty = false;
    node.agentOsLoaded = FS.isDir(mode);
    node.agentOsRemoteSize = 0;
    if (FS.isDir(mode)) {
      node.node_ops = workspaceDirNodeOps;
      node.stream_ops = workspaceDirStreamOps;
    } else if (FS.isFile(mode)) {
      node.node_ops = workspaceFileNodeOps;
      node.stream_ops = workspaceFileStreamOps;
    }
    return node;
  }

  function syncDirectory(node) {
    const guestPath = nodeGuestPath(node);
    const entries = withFsErrors(() => bridge.fsReaddirSync(guestPath));
    const remoteNames = new Set(entries);

    for (const name of Object.keys(node.contents || {})) {
      if (remoteNames.has(name)) {
        continue;
      }

      const child = node.contents[name];
      if (FS.isDir(child.mode)) {
        memfsDirNodeOps.rmdir(node, name);
      } else {
        memfsDirNodeOps.unlink(node, name);
      }
    }

    for (const name of entries) {
      const childPath = joinGuestPath(guestPath, name);
      const stat = withFsErrors(() => bridge.fsStatSync(childPath));
      const existing = node.contents[name];

      if (existing) {
        const existingIsDir = FS.isDir(existing.mode);
        const remoteIsDir = Boolean(stat?.isDirectory);
        if (existingIsDir !== remoteIsDir) {
          if (existingIsDir) {
            memfsDirNodeOps.rmdir(node, name);
          } else {
            memfsDirNodeOps.unlink(node, name);
          }
        } else {
          existing.agentOsGuestPath = childPath;
          updateNodeFromRemoteStat(existing, stat);
          if (FS.isFile(existing.mode) && !existing.agentOsDirty) {
            existing.agentOsLoaded = false;
          }
          continue;
        }
      }

      const mode = stat?.mode ?? (stat?.isDirectory ? 0o040755 : 0o100644);
      const child = createWorkspaceNode(node, name, mode, 0, childPath);
      updateNodeFromRemoteStat(child, stat);
    }
  }

  function loadFileContents(node) {
    if (node.agentOsDirty) {
      return;
    }

    const stat = withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
    updateNodeFromRemoteStat(node, stat);
    const contentBase64 = withFsErrors(() => bridge.fsReadSync(nodeGuestPath(node)));
    const bytes = Uint8Array.from(Buffer.from(contentBase64, 'base64'));
    node.contents = bytes;
    node.usedBytes = bytes.length;
    node.agentOsLoaded = true;
    node.agentOsRemoteSize = bytes.length;
  }

  function persistFile(node) {
    const contents = node.contents ? MEMFS.getFileDataAsTypedArray(node) : new Uint8Array(0);
    withFsErrors(() => bridge.fsWriteSync(nodeGuestPath(node), contents));
    node.agentOsDirty = false;
    node.agentOsLoaded = true;
    node.agentOsRemoteSize = contents.length;
    node.timestamp = Date.now();
  }

  function makeStat(node, stat) {
    const mode = stat?.mode ?? node.mode;
    const size = FS.isDir(mode) ? 4096 : (node.agentOsDirty ? node.usedBytes : (stat?.size ?? node.usedBytes ?? 0));
    const timestamp = new Date(node.timestamp || Date.now());

    return {
      dev: 1,
      ino: node.id,
      mode,
      nlink: FS.isDir(mode) ? 2 : 1,
      uid: 0,
      gid: 0,
      rdev: 0,
      size,
      atime: timestamp,
      mtime: timestamp,
      ctime: timestamp,
      blksize: 4096,
      blocks: Math.max(1, Math.ceil(size / 4096)),
    };
  }

  const workspaceFileNodeOps = {
    getattr(node) {
      const stat = node.agentOsDirty
        ? null
        : withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
      if (stat) {
        updateNodeFromRemoteStat(node, stat);
      }
      return makeStat(node, stat);
    },
    setattr(node, attr) {
      memfsFileNodeOps.setattr(node, attr);
      if (attr?.size != null) {
        node.agentOsDirty = true;
        node.agentOsLoaded = true;
      }
    },
  };

  const workspaceFileStreamOps = {
    llseek(stream, offset, whence) {
      return memfsFileStreamOps.llseek(stream, offset, whence);
    },
    read(stream, buffer, offset, length, position) {
      if (!stream.node.agentOsLoaded && !stream.node.agentOsDirty) {
        loadFileContents(stream.node);
      }
      return memfsFileStreamOps.read(stream, buffer, offset, length, position);
    },
    write(stream, buffer, offset, length, position, canOwn) {
      if (!stream.node.agentOsLoaded && !stream.node.agentOsDirty) {
        loadFileContents(stream.node);
      }
      const written = memfsFileStreamOps.write(stream, buffer, offset, length, position, canOwn);
      stream.node.agentOsDirty = true;
      persistFile(stream.node);
      return written;
    },
    mmap(stream, length, position, prot, flags) {
      if (!stream.node.agentOsLoaded && !stream.node.agentOsDirty) {
        loadFileContents(stream.node);
      }
      return memfsFileStreamOps.mmap(stream, length, position, prot, flags);
    },
    msync(stream, buffer, offset, length, mmapFlags) {
      const result = memfsFileStreamOps.msync(stream, buffer, offset, length, mmapFlags);
      stream.node.agentOsDirty = true;
      persistFile(stream.node);
      return result;
    },
  };

  const workspaceDirNodeOps = {
    getattr(node) {
      const stat = withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
      updateNodeFromRemoteStat(node, stat);
      return makeStat(node, stat);
    },
    setattr(node, attr) {
      memfsDirNodeOps.setattr(node, attr);
    },
    lookup(parent, name) {
      syncDirectory(parent);
      try {
        return memfsDirNodeOps.lookup(parent, name);
      } catch (error) {
        if (!(error instanceof FS.ErrnoError) || error.errno !== ERRNO_CODES.ENOENT) {
          throw error;
        }

        const guestPath = joinGuestPath(nodeGuestPath(parent), name);
        const stat = withFsErrors(() => bridge.fsStatSync(guestPath));
        const child = createWorkspaceNode(parent, name, stat.mode, 0, guestPath);
        updateNodeFromRemoteStat(child, stat);
        return child;
      }
    },
    mknod(parent, name, mode, dev) {
      const guestPath = joinGuestPath(nodeGuestPath(parent), name);
      const node = createWorkspaceNode(parent, name, mode, dev, guestPath);
      if (FS.isDir(mode)) {
        withFsErrors(() => bridge.fsMkdirSync(guestPath, { recursive: false }));
      } else if (FS.isFile(mode)) {
        node.contents = new Uint8Array(0);
        node.usedBytes = 0;
        node.agentOsDirty = true;
        persistFile(node);
      }
      return node;
    },
    rename() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
    unlink() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
    rmdir() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
    readdir(node) {
      syncDirectory(node);
      return memfsDirNodeOps.readdir(node);
    },
    symlink() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
  };

  try {
    FS.mkdir('/workspace');
  } catch (error) {
    if (!(error instanceof FS.ErrnoError) || error.errno !== ERRNO_CODES.EEXIST) {
      throw error;
    }
  }

  FS.mount(
    {
      mount(mount) {
        const root = MEMFS.mount(mount);
        root.agentOsGuestPath = mount.mountpoint;
        root.agentOsDirty = false;
        root.agentOsLoaded = true;
        root.agentOsRemoteSize = 0;
        root.node_ops = workspaceDirNodeOps;
        root.stream_ops = workspaceDirStreamOps;
        return root;
      },
    },
    {},
    '/workspace',
  );
}

async function readLockFileContents(indexPath, indexURL) {
  const { path: lockFilePath, url: lockFileUrl } = resolvePyodideResource(
    indexPath,
    indexURL,
    'pyodide-lock.json',
  );
  try {
    if (typeof lockFilePath === 'string' && lockFilePath.startsWith('/') && bridgeLoadFileSync) {
      return callBridgeSync(bridgeLoadFileSync, [lockFilePath]);
    }
    return await readFile(lockFilePath, 'utf8');
  } catch (error) {
    throw wrapPythonStartupError('lock file readFile', {
      indexPath,
      indexURL,
      lockFileUrl,
      lockFilePath,
    }, error);
  }
}

function installPythonStdin(pyodide) {
  if (typeof pyodide?.setStdin !== 'function') {
    return;
  }

  function readFromKernelStdin(buffer) {
    while (true) {
      const response = callBridgeSync(bridgeKernelStdinRead, [buffer.length, 100]);
      if (response?.done) {
        return 0;
      }

      const dataBase64 = typeof response?.dataBase64 === 'string' ? response.dataBase64 : '';
      if (dataBase64.length === 0) {
        continue;
      }

      const chunk = Buffer.from(dataBase64, 'base64');
      const target = Buffer.from(buffer.buffer, buffer.byteOffset, buffer.byteLength);
      chunk.copy(target, 0, 0, Math.min(chunk.length, target.length));
      return Math.min(chunk.length, buffer.length);
    }
  }

  pyodide.setStdin({
    isatty: false,
    read(buffer) {
      if (bridgeKernelStdinRead) {
        return readFromKernelStdin(buffer);
      }
      return readSync(STDIN_FD, buffer, 0, buffer.length, null);
    },
  });
}

function resolvePythonSource(pyodide) {
  const filePath = process.env[PYTHON_FILE_ENV];
  if (filePath != null) {
    if (typeof pyodide?.FS?.readFile !== 'function') {
      throw new Error(`Pyodide FS.readFile() is required to execute ${filePath}`);
    }

    return pyodide.FS.readFile(filePath, { encoding: 'utf8' });
  }

  return requiredEnv(PYTHON_CODE_ENV);
}

let pythonVfsRpcBridge = null;

try {
  const startupStarted = realPerformance.now();
  emitWarmupStage('startup');
  emitWarmupStage(`python-rpc-bridge:${bridgePythonRpc ? 'on' : 'off'}`);
  const { indexPath, indexUrl } = resolveIndexLocation(requiredEnv(PYODIDE_INDEX_URL_ENV));
  const bundledPackageBaseUrl = normalizeBaseUrl(indexPath);
  const packageBaseUrl = normalizeBaseUrl(
    process.env[PYODIDE_PACKAGE_BASE_URL_ENV] ?? bundledPackageBaseUrl,
  );
  const packageCacheDir = resolvePyodidePackageCacheDir();
  emitWarmupStage(`package-cache-dir:${packageCacheDir}`);
  const prewarmOnly = process.env[PYTHON_PREWARM_ONLY_ENV] === '1';
  const preloadPackages = parsePreloadPackages(process.env[PYTHON_PRELOAD_PACKAGES_ENV]);
  const lockFileContents = await readLockFileContents(indexPath, indexUrl).catch((error) => {
    throw wrapPythonStartupError('lock file read', { indexPath, indexUrl }, error);
  });
  emitWarmupStage('lock-file-ready');
  const { url: pyodideModuleUrl } = resolvePyodideResource(indexPath, indexUrl, 'pyodide.mjs');
  const restorePyodideShellCompat = installPyodideShellCompat();
  const { loadPyodide } = await import(pyodideModuleUrl).catch((error) => {
    throw wrapPythonStartupError('module import', { indexPath, indexUrl, pyodideModuleUrl }, error);
  });
  emitWarmupStage('module-imported');

  if (typeof loadPyodide !== 'function') {
    throw new Error(`pyodide.mjs at ${indexUrl} does not export loadPyodide()`);
  }

  if (prewarmOnly) {
    const stdlibResource = resolvePyodideResource(indexPath, indexUrl, 'python_stdlib.zip');
    const wasmResource = resolvePyodideResource(indexPath, indexUrl, 'pyodide.asm.wasm');
    await readFile(stdlibResource.path);
    await readFile(wasmResource.path);
    restorePyodideShellCompat();
    emitWarmupStage('prewarm-assets-ready');
    emitPythonStartupMetrics({
      prewarmOnly: true,
      startupMs: realPerformance.now() - startupStarted,
      loadPyodideMs: 0,
      packageLoadMs: 0,
      packageCount: 0,
      source: 'prewarm',
    });
    process.exitCode = 0;
  } else {
  pythonVfsRpcBridge = installPythonVfsRpcBridge();
  installPythonGuestPreloadHardening(pythonVfsRpcBridge);
  mkdirSync(packageCacheDir, { recursive: true });
  emitWarmupStage('before-load-pyodide');
  const loadPyodideStarted = realPerformance.now();
  const pyodide = await loadPyodide({
    indexURL: indexPath,
    lockFileContents,
    packageBaseUrl: bundledPackageBaseUrl,
    packageCacheDir,
    env: buildPythonRuntimeEnv(),
    stdout: writePyodideStdout,
    stderr: (message) => writeStream(process.stderr, message),
  }).catch((error) => {
    throw wrapPythonStartupError(
      'Pyodide bootstrap',
      {
        indexPath,
        indexUrl,
        packageBaseUrl,
        bundledPackageBaseUrl,
        packageCacheDir,
        pyodideModuleUrl,
      },
      error,
    );
  });
  restorePyodideShellCompat();
  emitWarmupStage('after-load-pyodide');
  const loadPyodideMs = realPerformance.now() - loadPyodideStarted;
  let packageLoadMs = 0;

  installPythonStdin(pyodide);
  installPythonWorkspaceFs(pyodide, pythonVfsRpcBridge);
  installPythonGuestLoaderHooks();
  if (pyodide?._api?.config) {
    pyodide._api.config.packageBaseUrl = bundledPackageBaseUrl;
    emitWarmupStage(`pyodide-package-base:${pyodide._api.config.packageBaseUrl}`);
  }
  const canLoadPackages = typeof pyodide?.loadPackage === 'function';
  if (!canLoadPackages && preloadPackages.length > 0) {
    throw new Error('Pyodide loadPackage() is required to preload Python packages');
  }
  if (canLoadPackages) {
    emitWarmupStage('before-load-micropip');
    await pyodide.loadPackage(['micropip']);
    emitWarmupStage('after-load-micropip');
    if (preloadPackages.length > 0) {
      emitWarmupStage('before-load-preload-packages');
      const packageLoadStarted = realPerformance.now();
      await pyodide.loadPackage(preloadPackages);
      packageLoadMs = realPerformance.now() - packageLoadStarted;
      emitWarmupStage('after-load-preload-packages');
    }
  }
  if (pyodide?._api?.config) {
    pyodide._api.config.packageBaseUrl = packageBaseUrl;
    emitWarmupStage(`micropip-package-base:${pyodide._api.config.packageBaseUrl}`);
  }
  installPythonMicropipCompat(pyodide);
  installPythonKernelRpcShims(pyodide);
  installPythonGuestProcessHardening();
  installPythonGuestImportBlocklist(pyodide);
  installPythonRuntimeEnv(pyodide);
  const source = process.env[PYTHON_FILE_ENV] != null ? 'file' : 'inline';
  emitPythonStartupMetrics({
    prewarmOnly: false,
    startupMs: realPerformance.now() - startupStarted,
    loadPyodideMs,
    packageLoadMs,
    packageCount: preloadPackages.length,
    source,
  });
  const code = resolvePythonSource(pyodide);
  await pyodide.runPythonAsync(code);
  }
} catch (error) {
  writeStream(process.stderr, formatError(error));
  process.exitCode = 1;
} finally {
  pythonVfsRpcBridge?.dispose();
  emitControlMessage({ type: 'python_exit', exitCode: process.exitCode ?? 0 });
}
process.exit(process.exitCode ?? 0);
