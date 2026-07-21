import { closeSync, createReadStream, mkdirSync, readFileSync, readSync, writeSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import * as moduleBuiltin from 'node:module';
import { performance as realPerformance } from 'node:perf_hooks';
import path from 'node:path';
import readline from 'node:readline';
import { URL } from 'node:url';

const ACCESS_DENIED_CODE = 'ERR_ACCESS_DENIED';
const ASSET_ROOT_ENV = 'AGENTOS_NODE_IMPORT_CACHE_ASSET_ROOT';
const PYODIDE_INDEX_URL_ENV = 'AGENTOS_PYODIDE_INDEX_URL';
const PYODIDE_PACKAGE_BASE_URL_ENV = 'AGENTOS_PYODIDE_PACKAGE_BASE_URL';
const PYODIDE_PACKAGE_CACHE_DIR_ENV = 'AGENTOS_PYODIDE_PACKAGE_CACHE_DIR';
const PYODIDE_PACKAGE_CACHE_GUEST_ROOT = '/__agentos_pyodide_cache';
const PYTHON_CODE_ENV = 'AGENTOS_PYTHON_CODE';
const PYTHON_FILE_ENV = 'AGENTOS_PYTHON_FILE';
const PYTHON_ARGV_ENV = 'AGENTOS_PYTHON_ARGV';
const PYTHON_MODULE_ENV = 'AGENTOS_PYTHON_MODULE';
const PYTHON_STDIN_PROGRAM_ENV = 'AGENTOS_PYTHON_STDIN_PROGRAM';
const PYTHON_INTERACTIVE_ENV = 'AGENTOS_PYTHON_INTERACTIVE';
const PYTHON_PREWARM_ONLY_ENV = 'AGENTOS_PYTHON_PREWARM_ONLY';
const RETAIN_LANGUAGE_CONTEXT_ENV = 'AGENTOS_RETAIN_LANGUAGE_CONTEXT';
const PYTHON_WARMUP_DEBUG_ENV = 'AGENTOS_PYTHON_WARMUP_DEBUG';
const PYTHON_WARMUP_METRICS_PREFIX = '__AGENTOS_PYTHON_WARMUP_METRICS__:';
const PYTHON_PRELOAD_PACKAGES_ENV = 'AGENTOS_PYTHON_PRELOAD_PACKAGES';
const PYTHON_VFS_RPC_REQUEST_FD_ENV = 'AGENTOS_PYTHON_VFS_RPC_REQUEST_FD';
const PYTHON_VFS_RPC_RESPONSE_FD_ENV = 'AGENTOS_PYTHON_VFS_RPC_RESPONSE_FD';
const FORWARD_KERNEL_STDIN_RPC_ENV = 'AGENTOS_FORWARD_KERNEL_STDIN_RPC';
const INTERNAL_ENV = globalThis.__agentOSPythonInternalEnv ?? Object.create(null);
const ALLOW_PROCESS_BINDINGS = readRunnerEnv('AGENTOS_ALLOW_PROCESS_BINDINGS') === '1';
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
const PYTHON_STDIN_DONE_SENTINEL = '__AGENTOS_PYTHON_STDIN_DONE__';
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
const bridgePythonStdinRead =
  readRunnerEnv(FORWARD_KERNEL_STDIN_RPC_ENV) !== '1' &&
  canCallBridgeSync(globalThis._pythonStdinRead)
    ? globalThis._pythonStdinRead
    : null;
const bridgeKernelStdinRead =
  globalThis._kernelStdinReadRaw ?? globalThis._kernelStdinRead ?? null;
const bridgeLoadFileSync =
  canCallBridgeSync(globalThis._loadFileSync)
    ? globalThis._loadFileSync
    : null;
const originalGetBuiltinModule =
  typeof process.getBuiltinModule === 'function'
    ? process.getBuiltinModule.bind(process)
    : null;
const CONTROL_PIPE_FD = parseControlPipeFd(readRunnerEnv('AGENTOS_CONTROL_PIPE_FD'));
const register = typeof moduleBuiltin?.register === 'function' ? moduleBuiltin.register.bind(moduleBuiltin) : null;

function readRunnerEnv(name) {
  const internalValue = INTERNAL_ENV[name];
  if (typeof internalValue === 'string') {
    return internalValue;
  }
  return process.env[name];
}

function requiredEnv(name) {
  const value = readRunnerEnv(name);
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
  const configured = readRunnerEnv(PYODIDE_PACKAGE_CACHE_DIR_ENV);
  if (typeof configured === 'string' && configured.trim() !== '') {
    return path.resolve(configured);
  }

  return PYODIDE_PACKAGE_CACHE_GUEST_ROOT;
}

function emitWarmupStage(stage) {
  if (readRunnerEnv(PYTHON_WARMUP_DEBUG_ENV) !== '1') {
    return;
  }

  writeStream(process.stderr, `__AGENTOS_PYTHON_WARMUP_STAGE__:${stage}`);
}

function emitPythonDebug(channel, message) {
  if (readRunnerEnv(PYTHON_WARMUP_DEBUG_ENV) !== '1') {
    return;
  }

  writeStream(process.stderr, `__AGENTOS_PYTHON_${channel}__:${message}`);
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

  throw new Error('unsupported fetch body type for agentos Python package loading');
}

function emitPythonStartupMetrics({
  prewarmOnly,
  startupMs,
  loadPyodideMs,
  packageLoadMs,
  packageCount,
  source,
}) {
  if (readRunnerEnv(PYTHON_WARMUP_DEBUG_ENV) !== '1') {
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
  const value = readRunnerEnv(name);
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
    normalized.code = 'ERR_AGENTOS_PYTHON_VFS_RPC';
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
    fsLstatSync(path) {
      const result = requestSync('fsLstat', { path });
      return result.stat ?? null;
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
    fsUnlinkSync(path) {
      requestSync('fsUnlink', { path });
    },
    fsRmdirSync(path) {
      requestSync('fsRmdir', { path });
    },
    fsRenameSync(path, destination) {
      requestSync('fsRename', { path, destination });
    },
    fsSymlinkSync(target, path) {
      requestSync('fsSymlink', { target, path });
    },
    fsReadlinkSync(path) {
      const result = requestSync('fsReadlink', { path });
      return result.target ?? '';
    },
    fsSetattrSync(path, attr) {
      requestSync('fsSetattr', { path, ...attr });
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
      argv0 = null,
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
        argv0,
        cwd,
        env,
        shell,
        maxBuffer,
      }));
    },
    socketConnectSync(host, port) {
      return JSON.stringify(requestSync('socketConnect', { hostname: host, port }));
    },
    socketSendSync(socketId, dataBase64) {
      return JSON.stringify(requestSync('socketSend', { socketId, bodyBase64: dataBase64 }));
    },
    socketRecvSync(socketId, maxBuffer, timeoutMs) {
      return JSON.stringify(requestSync('socketRecv', { socketId, maxBuffer, timeoutMs }));
    },
    socketCloseSync(socketId) {
      return JSON.stringify(requestSync('socketClose', { socketId }));
    },
    udpCreateSync() {
      return JSON.stringify(requestSync('udpCreate', {}));
    },
    udpSendtoSync(socketId, host, port, dataBase64) {
      return JSON.stringify(
        requestSync('udpSendto', { socketId, hostname: host, port, bodyBase64: dataBase64 }),
      );
    },
    udpRecvfromSync(socketId, maxBuffer, timeoutMs) {
      return JSON.stringify(requestSync('udpRecvfrom', { socketId, maxBuffer, timeoutMs }));
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
        throw new Error('agentos Python VFS RPC response channel closed unexpectedly');
      }
      responseBuffer += chunk.subarray(0, bytesRead).toString('utf8');
    }
  }

  function parseResponseLine(line) {
    try {
      return JSON.parse(line);
    } catch (error) {
      throw new Error(`invalid agentos Python VFS RPC response: ${formatError(error)}`);
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

    const error = new Error(message?.error?.message || `agentos Python VFS RPC request ${id} failed`);
    error.code = message?.error?.code || 'ERR_AGENTOS_PYTHON_VFS_RPC';
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
    fsLstatSync(path) {
      const result = requestSync('fsLstat', { path });
      return result.stat ?? null;
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
    fsUnlinkSync(path) {
      requestSync('fsUnlink', { path });
    },
    fsRmdirSync(path) {
      requestSync('fsRmdir', { path });
    },
    fsRenameSync(path, destination) {
      requestSync('fsRename', { path, destination });
    },
    fsSymlinkSync(target, path) {
      requestSync('fsSymlink', { target, path });
    },
    fsReadlinkSync(path) {
      const result = requestSync('fsReadlink', { path });
      return result.target ?? '';
    },
    fsSetattrSync(path, attr) {
      requestSync('fsSetattr', { path, ...attr });
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
      argv0 = null,
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
        argv0,
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
  const error = new Error(`${subject} is not available in the agentos guest Python runtime`);
  error.code = ACCESS_DENIED_CODE;
  return error;
}

const PYTHON_GUEST_IMPORT_BLOCKLIST_SOURCE = String.raw`
import builtins as _agentos_builtins
import sys as _agentos_sys
import types as _agentos_types

try:
    import agentos_internal_js as _agentos_safe_js
    import agentos_internal_pyodide_js as _agentos_safe_pyodide_js
    import agentos_internal_pyodide_js_api as _agentos_safe_pyodide_js_api
except Exception:
    _agentos_safe_js = None
    _agentos_safe_pyodide_js = None
    _agentos_safe_pyodide_js_api = None

def _agentos_raise_access_denied(module_name):
    raise RuntimeError(f"{module_name} is not available in the agentos guest Python runtime")

class _AgentOsBlockedModule(_agentos_types.ModuleType):
    def __init__(self, name):
        super().__init__(name)
        self.__dict__['__all__'] = ()

    def __getattr__(self, _name):
        _agentos_raise_access_denied(self.__name__)

    def __dir__(self):
        return []

_agentos_blocked_modules = {
    _agentos_module_name: _AgentOsBlockedModule(_agentos_module_name)
    for _agentos_module_name in ('js', 'pyodide_js')
}

_agentos_safe_modules = {
    "js": _agentos_safe_js,
    "pyodide_js": _agentos_safe_pyodide_js,
    "pyodide_js._api": _agentos_safe_pyodide_js_api,
}

_agentos_original_import = _agentos_builtins.__import__

def _agentos_allow_internal_js(globals):
    module_name = str((globals or {}).get("__name__", ""))
    return module_name.startswith("micropip") or module_name.startswith("pyodide.http")

def _agentos_import(name, globals=None, locals=None, fromlist=(), level=0):
    if name in _agentos_safe_modules and _agentos_safe_modules[name] is not None and _agentos_allow_internal_js(globals):
        return _agentos_safe_modules[name]
    if name in _agentos_blocked_modules:
        return _agentos_blocked_modules[name]
    return _agentos_original_import(name, globals, locals, fromlist, level)

_agentos_builtins.__import__ = _agentos_import
_agentos_sys.modules.update(_agentos_blocked_modules)
`;

const PYTHON_KERNEL_RPC_SHIMS_SOURCE = String.raw`
import base64 as _agentos_base64
import json as _agentos_json
import socket as _agentos_socket
import subprocess as _agentos_subprocess
import sys as _agentos_sys
import types as _agentos_types
import urllib.error as _agentos_urllib_error
import urllib.request as _agentos_urllib_request
from email.message import Message as _AgentOsMessage
from js import __agentOSPythonVfsRpc as _agentos_rpc

def _agentos_raise_from_error(error):
    if not isinstance(error, dict):
        raise RuntimeError(str(error))
    message = str(error.get("message", "agentos Python bridge request failed"))
    if "EACCES:" in message:
        raise PermissionError(message)
    if "command not found" in message:
        raise FileNotFoundError(message)
    raise OSError(message)

def _agentos_normalize_family(family):
    if family in (None, 0):
        return None
    if family == _agentos_socket.AF_INET:
        return 4
    if family == _agentos_socket.AF_INET6:
        return 6
    return None

def _agentos_dns_lookup(hostname, family=None):
    try:
        result = _agentos_json.loads(
            _agentos_rpc.dnsLookupSync(hostname, _agentos_normalize_family(family))
        )
    except Exception as error:
        _agentos_raise_from_error({"message": str(error)})
    addresses = result.get("addresses") or []
    if not addresses:
        raise OSError(f"agentos DNS lookup returned no addresses for {hostname}")
    return addresses

class _AgentOsHttpResponse:
    def __init__(self, payload):
        self.status = int(payload.get("status", 0))
        self.reason = str(payload.get("reason", ""))
        self.url = str(payload.get("url", ""))
        self._body = _agentos_base64.b64decode(payload.get("bodyBase64", "") or "")
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
        self._body = _agentos_base64.b64decode(payload.get("bodyBase64", "") or "")

    async def bytes(self):
        return self._body

    async def string(self):
        return self._body.decode("utf-8", errors="replace")

    def raise_for_status(self):
        if self.status >= 400:
            raise RuntimeError(f"{self.status} {self.status_text}")

def _agentos_extract_request_parts(url_or_request, data=None):
    if isinstance(url_or_request, _agentos_urllib_request.Request):
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
        body_base64 = _agentos_base64.b64encode(payload).decode("ascii")
    return url, method, headers, body_base64

def _agentos_http_request(url_or_request, data=None):
    url, method, headers, body_base64 = _agentos_extract_request_parts(url_or_request, data)
    try:
        payload = _agentos_json.loads(
            _agentos_rpc.httpRequestSync(url, method, _agentos_json.dumps(headers), body_base64)
        )
    except Exception as error:
        _agentos_raise_from_error({"message": str(error)})
    response = _AgentOsHttpResponse(payload)
    if response.status >= 400:
        raise _agentos_urllib_error.HTTPError(
            url,
            response.status,
            response.reason,
            response.headers,
            response,
        )
    return response

async def _agentos_pyfetch(url, **kwargs):
    headers = dict(kwargs.get("headers") or {})
    method = str(kwargs.get("method", "GET")).upper()
    body = kwargs.get("body")
    if body is not None and isinstance(body, str):
        body = body.encode("utf-8")
    body_base64 = None if body is None else _agentos_base64.b64encode(body).decode("ascii")
    try:
        payload = _agentos_json.loads(
            _agentos_rpc.httpRequestSync(
                str(url),
                method,
                _agentos_json.dumps(headers),
                body_base64,
            )
        )
    except Exception as error:
        _agentos_raise_from_error({"message": str(error)})
    return _AgentOsPyfetchResponse(payload)

def _agentos_urlopen(url, data=None, timeout=None, *args, **kwargs):
    del timeout, args, kwargs
    return _agentos_http_request(url, data=data)

_agentos_urllib_request.urlopen = _agentos_urlopen

try:
    import pyodide.http as _agentos_pyodide_http
except ModuleNotFoundError:
    _agentos_pyodide_http = None
else:
    _agentos_pyodide_http.pyfetch = _agentos_pyfetch

_agentos_original_getaddrinfo = _agentos_socket.getaddrinfo

def _agentos_getaddrinfo(host, port, family=0, type=0, proto=0, flags=0):
    if host in (None, "", "0.0.0.0", "::"):
        return _agentos_original_getaddrinfo(host, port, family, type, proto, flags)
    addresses = _agentos_dns_lookup(host, family)
    socktype = type or _agentos_socket.SOCK_STREAM
    protocol = proto or 0
    normalized_family = family or _agentos_socket.AF_INET
    results = []
    for address in addresses:
        entry_family = _agentos_socket.AF_INET6 if ":" in address else _agentos_socket.AF_INET
        if family not in (0, entry_family):
            continue
        if entry_family == _agentos_socket.AF_INET6:
            sockaddr = (address, port, 0, 0)
        else:
            sockaddr = (address, port)
        results.append((entry_family, socktype, protocol, "", sockaddr))
    if not results:
        raise OSError(f"agentos DNS lookup returned no matching addresses for {host}")
    return results

def _agentos_gethostbyname(host):
    return _agentos_dns_lookup(host, _agentos_socket.AF_INET)[0]

_agentos_socket.getaddrinfo = _agentos_getaddrinfo
_agentos_socket.gethostbyname = _agentos_gethostbyname

# Raw socket bridge: back socket.socket() with the host (outbound TCP + UDP).
# Reads poll (the host uses a short read timeout) so the synchronous RPC never
# stalls the sidecar; the loop below re-polls to emulate blocking semantics.
import base64 as _agentos_base64
import time as _agentos_time
import errno as _agentos_errno

_agentos_original_socket_class = _agentos_socket.socket

def _agentos_socket_oserror(exc):
    # Host errors arrive as "E<NAME>: message"; recover the errno so Python
    # code can catch ConnectionRefusedError/TimeoutError/etc. (OSError picks the
    # right subclass from the errno).
    message = str(getattr(exc, "message", None) or exc)
    head = message.split(":", 1)[0].strip()
    code = getattr(_agentos_errno, head, 0) if head[:1] == "E" and head.isupper() else 0
    return OSError(code or 0, message)

def _agentos_socket_rpc(call):
    try:
        return _agentos_json.loads(call())
    except OSError:
        raise
    except Exception as exc:
        raise _agentos_socket_oserror(exc) from None

class _AgentOsSocket:
    def __init__(self, family=None, type=None, proto=0, fileno=None):
        self.family = family if family is not None else _agentos_socket.AF_INET
        self.type = type if type is not None else _agentos_socket.SOCK_STREAM
        self.proto = proto
        self._timeout = None  # None blocks; 0 is non-blocking; >0 is a deadline
        self._id = None
        self._closed = False
        self._is_udp = self.type == _agentos_socket.SOCK_DGRAM
        if self._is_udp:
            resp = _agentos_socket_rpc(lambda: _agentos_rpc.udpCreateSync())
            self._id = int(resp["socketId"])

    def connect(self, address):
        host, port = address[0], address[1]
        resp = _agentos_socket_rpc(lambda: _agentos_rpc.socketConnectSync(str(host), int(port)))
        self._id = int(resp["socketId"])

    def connect_ex(self, address):
        try:
            self.connect(address)
            return 0
        except OSError as exc:
            return exc.errno or 1

    def _ensure_id(self):
        if self._id is None:
            raise OSError(9, "Bad file descriptor")
        return self._id

    def send(self, data, flags=0):
        sid = self._ensure_id()
        b64 = _agentos_base64.b64encode(bytes(data)).decode("ascii")
        resp = _agentos_socket_rpc(lambda: _agentos_rpc.socketSendSync(sid, b64))
        return int(resp.get("bytesSent", len(data)))

    def sendall(self, data, flags=0):
        payload = bytes(data)
        total = 0
        while total < len(payload):
            total += self.send(payload[total:], flags)
        return None

    def _poll(self, bufsize, recv_fn):
        deadline = None
        if self._timeout is not None and self._timeout > 0:
            deadline = _agentos_time.monotonic() + self._timeout
        backoff = 0.0
        while True:
            if self._timeout == 0:
                wait_ms = 0
            elif deadline is None:
                wait_ms = 30000
            else:
                wait_ms = max(0, int((deadline - _agentos_time.monotonic()) * 1000))
            resp = _agentos_socket_rpc(lambda: recv_fn(int(bufsize), wait_ms))
            if resp.get("closed"):
                return b"", resp
            data = resp.get("dataBase64") or ""
            if data:
                return _agentos_base64.b64decode(data), resp
            if resp.get("timedOut"):
                if self._timeout == 0:
                    raise BlockingIOError(11, "Resource temporarily unavailable")
                if deadline is not None and _agentos_time.monotonic() >= deadline:
                    raise _agentos_socket.timeout("timed out")
                # Guest-side capped backoff so a blocking recv on a silent socket
                # doesn't hammer the host loop with back-to-back polls.
                if backoff:
                    _agentos_time.sleep(backoff)
                backoff = min(backoff * 2 if backoff else 0.005, 0.05)
                continue
            return b"", resp

    def recv(self, bufsize, flags=0):
        sid = self._ensure_id()
        data, _ = self._poll(
            bufsize, lambda n, wait_ms: _agentos_rpc.socketRecvSync(sid, n, wait_ms)
        )
        return data

    def sendto(self, data, *args):
        address = args[-1]
        host, port = address[0], address[1]
        if self._id is None:
            resp = _agentos_socket_rpc(lambda: _agentos_rpc.udpCreateSync())
            self._id = int(resp["socketId"])
        b64 = _agentos_base64.b64encode(bytes(data)).decode("ascii")
        resp = _agentos_socket_rpc(
            lambda: _agentos_rpc.udpSendtoSync(self._id, str(host), int(port), b64)
        )
        return int(resp.get("bytesSent", len(data)))

    def recvfrom(self, bufsize, flags=0):
        sid = self._ensure_id()
        data, resp = self._poll(
            bufsize, lambda n, wait_ms: _agentos_rpc.udpRecvfromSync(sid, n, wait_ms)
        )
        addr = (resp.get("host", ""), int(resp.get("port", 0))) if resp else ("", 0)
        return data, addr

    def settimeout(self, value):
        self._timeout = value

    def gettimeout(self):
        return self._timeout

    def setblocking(self, flag):
        self._timeout = None if flag else 0.0

    def setsockopt(self, *args, **kwargs):
        return None

    def getsockopt(self, *args, **kwargs):
        return 0

    def fileno(self):
        return self._id if self._id is not None else -1

    def getpeername(self):
        return ("", 0)

    def getsockname(self):
        return ("0.0.0.0", 0)

    def close(self):
        if self._closed:
            return
        self._closed = True
        if self._id is not None:
            try:
                _agentos_rpc.socketCloseSync(self._id)
            except Exception:
                pass
            self._id = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def __del__(self):
        try:
            self.close()
        except Exception:
            pass

def _agentos_socket_factory(family=-1, type=-1, proto=0, fileno=None):
    fam = family if family != -1 else _agentos_socket.AF_INET
    typ = type if type != -1 else _agentos_socket.SOCK_STREAM
    if (
        fileno is None
        and fam in (_agentos_socket.AF_INET, _agentos_socket.AF_INET6)
        and typ in (_agentos_socket.SOCK_STREAM, _agentos_socket.SOCK_DGRAM)
    ):
        return _AgentOsSocket(fam, typ, proto)
    return _agentos_original_socket_class(family, type, proto, fileno)

_agentos_socket.socket = _agentos_socket_factory

class _AgentOsRequestsResponse:
    def __init__(self, payload):
        self.status_code = int(payload.get("status", 0))
        self.reason = str(payload.get("reason", ""))
        self.url = str(payload.get("url", ""))
        self.headers = {str(name): ", ".join(values) for name, values in (payload.get("headers") or {}).items()}
        self.content = _agentos_base64.b64decode(payload.get("bodyBase64", "") or "")
        self.encoding = "utf-8"
        self.ok = self.status_code < 400

    @property
    def text(self):
        return self.content.decode(self.encoding, errors="replace")

    def json(self):
        return _agentos_json.loads(self.text)

    def raise_for_status(self):
        if self.status_code >= 400:
            raise RuntimeError(f"{self.status_code} {self.reason}")

class _AgentOsRequestsSession:
    def request(self, method, url, **kwargs):
        headers = dict(kwargs.get("headers") or {})
        data = kwargs.get("data")
        if data is not None and isinstance(data, str):
            data = data.encode("utf-8")
        body_base64 = None if data is None else _agentos_base64.b64encode(data).decode("ascii")
        try:
            payload = _agentos_json.loads(
                _agentos_rpc.httpRequestSync(
                    str(url),
                    str(method).upper(),
                    _agentos_json.dumps(headers),
                    body_base64,
                )
            )
        except Exception as error:
            _agentos_raise_from_error({"message": str(error)})
        return _AgentOsRequestsResponse(payload)

    def get(self, url, **kwargs):
        return self.request("GET", url, **kwargs)

def _agentos_install_requests_module():
    module = _agentos_types.ModuleType("requests")
    session = _AgentOsRequestsSession
    module.Session = session
    module.Response = _AgentOsRequestsResponse
    module.request = lambda method, url, **kwargs: session().request(method, url, **kwargs)
    module.get = lambda url, **kwargs: session().get(url, **kwargs)
    module.exceptions = _agentos_types.SimpleNamespace(RequestException=RuntimeError)
    _agentos_sys.modules["requests"] = module

try:
    import requests as _agentos_requests
except ModuleNotFoundError:
    _agentos_install_requests_module()
else:
    _agentos_requests.Session = _AgentOsRequestsSession
    _agentos_requests.Response = _AgentOsRequestsResponse
    _agentos_requests.request = lambda method, url, **kwargs: _AgentOsRequestsSession().request(method, url, **kwargs)
    _agentos_requests.get = lambda url, **kwargs: _AgentOsRequestsSession().get(url, **kwargs)

class _AgentOsCompletedProcess:
    def __init__(self, args, returncode, stdout, stderr):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def _agentos_subprocess_run(args, *, capture_output=False, check=False, cwd=None, env=None, input=None, shell=False, executable=None, text=False, encoding="utf-8", errors="strict", stdout=None, stderr=None, timeout=None, **kwargs):
    del kwargs, stdout, stderr, timeout
    if isinstance(args, (str, bytes)):
        original_command = args.decode("utf-8") if isinstance(args, bytes) else args
        values = None
    else:
        values = list(args)
        if not values:
            raise ValueError("subprocess.run args must not be empty")
        original_command = str(values[0])

    bridge_shell = False
    if shell:
        # Python implements shell=True as shell -c command [argv...]. Invoke the
        # shell explicitly so a string-valued executable selects the requested
        # shell and sequence arguments retain their Linux $0/$1 positions.
        command = str(executable) if executable is not None else "/bin/sh"
        argv0 = command
        if values is None:
            argv = ["-c", original_command]
        else:
            argv = ["-c", original_command, *[str(value) for value in values[1:]]]
    elif values is None:
        command = str(executable) if executable is not None else original_command
        argv0 = original_command
        argv = []
    else:
        command = str(executable) if executable is not None else original_command
        argv0 = original_command
        argv = [str(value) for value in values[1:]]
    merged_env = dict(env or {})
    resolved_cwd = cwd if cwd is not None else _agentos_os.environ.get("PWD")
    if input is not None:
        raise NotImplementedError("subprocess.run input is not supported in the agentos Python runtime")
    try:
        payload = _agentos_json.loads(
            _agentos_rpc.subprocessRunSync(
                command,
                _agentos_json.dumps(argv),
                argv0,
                resolved_cwd,
                _agentos_json.dumps(merged_env),
                bridge_shell,
            )
        )
    except Exception as error:
        _agentos_raise_from_error({"message": str(error)})
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
        raise _agentos_subprocess.CalledProcessError(
            result.returncode,
            args,
            output=result.stdout,
            stderr=result.stderr,
        )
    return result

_agentos_subprocess.run = _agentos_subprocess_run
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
  for (const [name, value] of Object.entries(process.env)) {
    if (
      typeof value === 'string' &&
      !name.startsWith('AGENTOS_') &&
      !name.startsWith('NODE_SYNC_RPC_')
    ) {
      runtimeEnv[name] = value;
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
import json as _agentos_json
import os as _agentos_os

for _agentos_key, _agentos_value in _agentos_json.loads(${JSON.stringify(JSON.stringify(runtimeEnv))}).items():
    _agentos_os.environ[_agentos_key] = _agentos_value
`);
}

function installPythonKernelRpcShims(pyodide) {
  if (typeof pyodide?.runPython !== 'function' || !globalThis.__agentOSPythonVfsRpc) {
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

  pyodide.registerJsModule('agentos_internal_js', {
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
  pyodide.registerJsModule('agentos_internal_pyodide_js', {
    loadedPackages: pyodide.loadedPackages,
    loadPackage: pyodide.loadPackage?.bind(pyodide),
    lockfileBaseUrl: pyodide?._api?.config?.packageBaseUrl ?? '',
    _api: pyodideApiCompat,
  });
  pyodide.registerJsModule('agentos_internal_pyodide_js_api', pyodideApiCompat);
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
  const assetRoot = readRunnerEnv(ASSET_ROOT_ENV);
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

  hardenProperty(globalThis, '__agentOSPythonVfsRpc', bridge);
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
  const memfsLinkNodeOps = MEMFS.ops_table.link.node;
  const workspaceDirStreamOps = memfsDirStreamOps;

  function joinGuestPath(parentPath, name) {
    return parentPath === '/' ? `/${name}` : `${parentPath}/${name}`;
  }

  function nodeGuestPath(node) {
    return node.agentOSGuestPath || node.mount?.mountpoint || '/workspace';
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
    if (FS.isFile(stat.mode) && !node.agentOSDirty) {
      node.agentOSRemoteSize = stat.size;
    }
  }

  function createWorkspaceNode(parent, name, mode, dev, guestPath) {
    const node = MEMFS.createNode(parent, name, mode, dev);
    node.agentOSGuestPath = guestPath;
    node.agentOSDirty = false;
    node.agentOSLoaded = FS.isDir(mode);
    node.agentOSRemoteSize = 0;
    if (FS.isDir(mode)) {
      node.node_ops = workspaceDirNodeOps;
      node.stream_ops = workspaceDirStreamOps;
    } else if (FS.isLink(mode)) {
      node.node_ops = workspaceLinkNodeOps;
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
      // lstat (don't follow) so a host symlink is created as a link node.
      const stat = withFsErrors(() => bridge.fsLstatSync(childPath));
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
          existing.agentOSGuestPath = childPath;
          updateNodeFromRemoteStat(existing, stat);
          if (FS.isFile(existing.mode) && !existing.agentOSDirty) {
            existing.agentOSLoaded = false;
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
    if (node.agentOSDirty) {
      return;
    }

    const stat = withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
    updateNodeFromRemoteStat(node, stat);
    const contentBase64 = withFsErrors(() => bridge.fsReadSync(nodeGuestPath(node)));
    const bytes = Uint8Array.from(Buffer.from(contentBase64, 'base64'));
    node.contents = bytes;
    node.usedBytes = bytes.length;
    node.agentOSLoaded = true;
    node.agentOSRemoteSize = bytes.length;
  }

  function persistFile(node) {
    const contents = node.contents ? MEMFS.getFileDataAsTypedArray(node) : new Uint8Array(0);
    withFsErrors(() => bridge.fsWriteSync(nodeGuestPath(node), contents));
    node.agentOSDirty = false;
    node.agentOSLoaded = true;
    node.agentOSRemoteSize = contents.length;
    node.timestamp = Date.now();
  }

  function makeStat(node, stat) {
    const mode = stat?.mode ?? node.mode;
    const size = FS.isDir(mode) ? 4096 : (node.agentOSDirty ? node.usedBytes : (stat?.size ?? node.usedBytes ?? 0));
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

  function toEpochMs(value) {
    if (value == null) return null;
    if (typeof value === 'number') return value;
    if (typeof value.getTime === 'function') return value.getTime();
    return null;
  }

  // Propagate chmod/chown/utimes from an Emscripten `setattr` into the host VFS.
  // (size/truncate is handled via the dirty-write path, not here.)
  function propagateSetattrToHost(node, attr) {
    if (!attr) return;
    const payload = {};
    if (attr.mode != null) payload.mode = attr.mode & 0o7777;
    // `os.chown(p, uid, -1)` keeps a side; never forward a negative sentinel
    // (it would saturate to 0 = root on the host).
    if (attr.uid != null && attr.uid >= 0) payload.uid = attr.uid;
    if (attr.gid != null && attr.gid >= 0) payload.gid = attr.gid;
    const atimeMs = toEpochMs(attr.atime ?? attr.timestamp);
    const mtimeMs = toEpochMs(attr.mtime ?? attr.timestamp);
    if (atimeMs != null && mtimeMs != null) {
      payload.atimeMs = Math.trunc(atimeMs);
      payload.mtimeMs = Math.trunc(mtimeMs);
    }
    if (Object.keys(payload).length === 0) return;
    withFsErrors(() => bridge.fsSetattrSync(nodeGuestPath(node), payload));
  }

  const workspaceLinkNodeOps = {
    // A symlink node reports itself (lstat semantics), not its target — so use
    // the in-memory link mode rather than a host stat (which follows the link).
    getattr(node) {
      return makeStat(node, null);
    },
    setattr(node, attr) {
      // Host first: if the host op fails (e.g. read-only root) it throws before
      // we mutate the in-isolate node, so the two views stay consistent.
      propagateSetattrToHost(node, attr);
      memfsLinkNodeOps.setattr(node, attr);
    },
    readlink(node) {
      return withFsErrors(() => bridge.fsReadlinkSync(nodeGuestPath(node)));
    },
  };

  const workspaceFileNodeOps = {
    getattr(node) {
      const stat = node.agentOSDirty
        ? null
        : withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
      if (stat) {
        updateNodeFromRemoteStat(node, stat);
      }
      return makeStat(node, stat);
    },
    setattr(node, attr) {
      // Host first (see link setattr) so a failed host op leaves the in-isolate
      // node untouched.
      propagateSetattrToHost(node, attr);
      memfsFileNodeOps.setattr(node, attr);
      if (attr?.size != null) {
        node.agentOSDirty = true;
        node.agentOSLoaded = true;
      }
    },
  };

  const workspaceFileStreamOps = {
    llseek(stream, offset, whence) {
      return memfsFileStreamOps.llseek(stream, offset, whence);
    },
    read(stream, buffer, offset, length, position) {
      if (!stream.node.agentOSLoaded && !stream.node.agentOSDirty) {
        loadFileContents(stream.node);
      }
      return memfsFileStreamOps.read(stream, buffer, offset, length, position);
    },
    write(stream, buffer, offset, length, position, canOwn) {
      if (!stream.node.agentOSLoaded && !stream.node.agentOSDirty) {
        loadFileContents(stream.node);
      }
      const written = memfsFileStreamOps.write(stream, buffer, offset, length, position, canOwn);
      stream.node.agentOSDirty = true;
      persistFile(stream.node);
      return written;
    },
    mmap(stream, length, position, prot, flags) {
      if (!stream.node.agentOSLoaded && !stream.node.agentOSDirty) {
        loadFileContents(stream.node);
      }
      return memfsFileStreamOps.mmap(stream, length, position, prot, flags);
    },
    msync(stream, buffer, offset, length, mmapFlags) {
      const result = memfsFileStreamOps.msync(stream, buffer, offset, length, mmapFlags);
      stream.node.agentOSDirty = true;
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
      // Host first (see link setattr).
      propagateSetattrToHost(node, attr);
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
        // lstat (don't follow) so a directly-looked-up host symlink is a link node.
        const stat = withFsErrors(() => bridge.fsLstatSync(guestPath));
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
        node.agentOSDirty = true;
        persistFile(node);
      }
      return node;
    },
    rename(oldNode, newDir, newName) {
      const source = nodeGuestPath(oldNode);
      const destination = joinGuestPath(nodeGuestPath(newDir), newName);
      withFsErrors(() => bridge.fsRenameSync(source, destination));
      // `nodeGuestPath` reads the stored path, so retarget the node before it
      // moves in the in-memory tree; children re-derive on the next sync.
      oldNode.agentOSGuestPath = destination;
      memfsDirNodeOps.rename(oldNode, newDir, newName);
    },
    unlink(parent, name) {
      withFsErrors(() =>
        bridge.fsUnlinkSync(joinGuestPath(nodeGuestPath(parent), name)),
      );
      if (parent.contents && Object.prototype.hasOwnProperty.call(parent.contents, name)) {
        memfsDirNodeOps.unlink(parent, name);
      }
    },
    rmdir(parent, name) {
      withFsErrors(() =>
        bridge.fsRmdirSync(joinGuestPath(nodeGuestPath(parent), name)),
      );
      if (parent.contents && Object.prototype.hasOwnProperty.call(parent.contents, name)) {
        memfsDirNodeOps.rmdir(parent, name);
      }
    },
    readdir(node) {
      syncDirectory(node);
      return memfsDirNodeOps.readdir(node);
    },
    symlink(parent, newName, oldPath) {
      const guestPath = joinGuestPath(nodeGuestPath(parent), newName);
      withFsErrors(() => bridge.fsSymlinkSync(oldPath, guestPath));
      const node = createWorkspaceNode(parent, newName, 0o120777, 0, guestPath);
      node.link = oldPath;
      node.usedBytes = oldPath.length;
      return node;
    },
  };

  const overlayBackend = {
    mount(mount) {
      const root = MEMFS.mount(mount);
      root.agentOSGuestPath = mount.mountpoint;
      root.agentOSDirty = false;
      root.agentOSLoaded = true;
      root.agentOSRemoteSize = 0;
      root.node_ops = workspaceDirNodeOps;
      root.stream_ops = workspaceDirStreamOps;
      return root;
    },
  };

  function mountVfsAt(guestPath) {
    try {
      FS.mkdir(guestPath);
    } catch (error) {
      if (!(error instanceof FS.ErrnoError) || error.errno !== ERRNO_CODES.EEXIST) {
        throw error;
      }
    }
    FS.mount(overlayBackend, {}, guestPath);
  }

  // Mount the kernel VFS over the VM's real top-level directories so Python sees
  // the whole guest filesystem — `/tmp`, `/etc`, `/root`, `/usr`, … — exactly
  // like the JS/WASM runtimes and `vm.readFile()`. Pyodide owns a handful of
  // paths in its own in-isolate FS; keep those on MEMFS so the interpreter, its
  // stdlib, and its devices keep working.
  const RESERVED_ROOTS = new Set([
    'lib',
    'dev',
    'proc',
    'home',
    '__agentos_pyodide',
    '__agentos_pyodide_cache',
  ]);
  let rootEntries = [];
  try {
    rootEntries = bridge.fsReaddirSync('/');
  } catch (error) {
    // A nested Python child can't reach the kernel VFS (it gets a recoverable
    // "unavailable" error and falls back to the in-isolate FS) — that case is
    // expected and quiet. Any other failure means the top-level process lost the
    // VM root, which is worth surfacing.
    if (!/not available for nested child/.test(String(error?.message ?? error))) {
      writeStream(
        process.stderr,
        `agentos: could not bridge the VM filesystem into Python (${formatError(error)}); only /workspace will be visible\n`,
      );
    }
    rootEntries = [];
  }
  for (const name of rootEntries) {
    if (RESERVED_ROOTS.has(name)) {
      continue;
    }
    const childPath = `/${name}`;
    let isDir = false;
    try {
      isDir = Boolean(bridge.fsStatSync(childPath)?.isDirectory);
    } catch {
      isDir = false;
    }
    if (!isDir) {
      continue;
    }
    try {
      mountVfsAt(childPath);
    } catch {
      // A path Pyodide owns or cannot shadow — skip it rather than abort boot.
    }
  }
  // /workspace stays available for backward compatibility even if the VM root
  // does not advertise it.
  if (!rootEntries.includes('workspace')) {
    mountVfsAt('/workspace');
  }
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
      if (bridgePythonStdinRead) {
        const response = callBridgeSync(bridgePythonStdinRead, [buffer.length, 100]);
        if (response === PYTHON_STDIN_DONE_SENTINEL) {
          return 0;
        }

        const dataBase64 = typeof response === 'string' ? response : '';
        if (dataBase64.length === 0) {
          continue;
        }

        const chunk = Buffer.from(dataBase64, 'base64');
        const target = Buffer.from(buffer.buffer, buffer.byteOffset, buffer.byteLength);
        chunk.copy(target, 0, 0, Math.min(chunk.length, target.length));
        return Math.min(chunk.length, buffer.length);
      }

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

function applyPythonArgv(pyodide) {
  const argvJson = readRunnerEnv(PYTHON_ARGV_ENV);
  if (argvJson == null) {
    return;
  }
  let argv;
  try {
    argv = JSON.parse(argvJson);
  } catch {
    return;
  }
  if (!Array.isArray(argv)) {
    return;
  }
  const normalized = argv.map((value) => String(value));
  pyodide.globals.set('__agentos_argv', pyodide.toPy(normalized));
  try {
    pyodide.runPython('import sys as _agentos_sys_argv\n_agentos_sys_argv.argv = list(__agentos_argv)\ndel _agentos_sys_argv');
  } finally {
    pyodide.globals.delete('__agentos_argv');
  }
}

// Drains the guest stdin stream to EOF and returns it as text. Used for
// `python -` (and piped programs), where stdin IS the program body.
function readProgramFromStdin() {
  const chunks = [];
  const CHUNK = 65536;
  if (bridgePythonStdinRead) {
    while (true) {
      const response = callBridgeSync(bridgePythonStdinRead, [CHUNK, 100]);
      if (response === PYTHON_STDIN_DONE_SENTINEL) {
        break;
      }
      const dataBase64 = typeof response === 'string' ? response : '';
      if (dataBase64.length === 0) {
        continue;
      }
      chunks.push(Buffer.from(dataBase64, 'base64'));
    }
  } else if (bridgeKernelStdinRead) {
    while (true) {
      const response = callBridgeSync(bridgeKernelStdinRead, [CHUNK, 100]);
      if (response?.done) {
        break;
      }
      const dataBase64 = typeof response?.dataBase64 === 'string' ? response.dataBase64 : '';
      if (dataBase64.length === 0) {
        continue;
      }
      chunks.push(Buffer.from(dataBase64, 'base64'));
    }
  } else {
    const buffer = Buffer.alloc(CHUNK);
    while (true) {
      let bytesRead = 0;
      try {
        bytesRead = readSync(STDIN_FD, buffer, 0, buffer.length, null);
      } catch {
        break;
      }
      if (bytesRead === 0) {
        break;
      }
      chunks.push(Buffer.from(buffer.subarray(0, bytesRead)));
    }
  }
  return Buffer.concat(chunks).toString('utf8');
}

// A persistent, kernel-VFS-backed site-packages. The default Pyodide
// site-packages lives in the per-process in-isolate MEMFS, so anything installed
// there vanishes when the process exits. This directory lives on the VM
// filesystem (via the kernel VFS), so `pip install` survives across separate
// `python` invocations and is visible to other processes — just like a real
// `site-packages`. It is prepended to `sys.path` on every boot.
const PYTHON_VFS_SITE_PACKAGES = '/root/.agentos/site-packages';

function installPythonVfsSitePackages(pyodide) {
  if (typeof pyodide?.runPython !== 'function') {
    return;
  }
  try {
    pyodide.globals.set('__agentos_vfs_site', PYTHON_VFS_SITE_PACKAGES);
    pyodide.runPython(
      'import os as _os, sys as _sys\n' +
        'try:\n' +
        '    _os.makedirs(__agentos_vfs_site, exist_ok=True)\n' +
        '    if __agentos_vfs_site not in _sys.path:\n' +
        // Append (not prepend): the stdlib + bundled packages stay first, so
        // hot imports resolve from the fast in-isolate FS and only genuinely
        // pip-installed packages incur a VFS lookup, and a VFS package can't
        // shadow the stdlib.
        '        _sys.path.append(__agentos_vfs_site)\n' +
        // Best-effort: if the VFS site-packages can't be created (e.g. a
        // read-only `/root`), persistence is simply unavailable — pip still
        // works in-process. Degrade quietly rather than spam stderr.
        'except OSError:\n' +
        '    pass\n' +
        'finally:\n' +
        '    del _os, _sys',
    );
  } catch (error) {
    writeStream(process.stderr, `agentos: VFS site-packages setup failed: ${formatError(error)}\n`);
  } finally {
    try {
      pyodide.globals.delete('__agentos_vfs_site');
    } catch (error) {
      writeStream(
        process.stderr,
        `agentos: VFS site-packages global cleanup failed: ${formatError(error)}\n`,
      );
    }
  }
}

// `pip` / `python -m pip`: emulate the common pip CLI via Pyodide's micropip,
// which fetches wheels through the runner's kernel-backed fetch (so egress is
// governed by the VM network policy, never an ambient host fetch). Installed
// packages are copied into the persistent VFS site-packages so they survive the
// per-process interpreter and can be imported by a later `python` invocation.
async function runPythonPip(pyodide) {
  pyodide.globals.set('__agentos_vfs_site', PYTHON_VFS_SITE_PACKAGES);
  try {
    await pyodide.runPythonAsync(`
import os, shutil, site, sys
_agentos_pip_args = sys.argv[1:]
if _agentos_pip_args and _agentos_pip_args[0] == "install":
    import micropip
    _agentos_pip_pkgs = [a for a in _agentos_pip_args[1:] if not a.startswith("-")]
    if not _agentos_pip_pkgs:
        print("ERROR: You must give at least one requirement to install", file=sys.stderr)
        sys.exit(1)
    _agentos_sp = site.getsitepackages()[0]
    _agentos_before = set(os.listdir(_agentos_sp)) if os.path.isdir(_agentos_sp) else set()
    await micropip.install(_agentos_pip_pkgs)
    # Persist whatever micropip extracted into the in-isolate site-packages into
    # the VFS-backed site-packages so it survives this process.
    os.makedirs(__agentos_vfs_site, exist_ok=True)
    _agentos_after = set(os.listdir(_agentos_sp)) if os.path.isdir(_agentos_sp) else set()
    for _agentos_name in sorted(_agentos_after - _agentos_before):
        _agentos_src = os.path.join(_agentos_sp, _agentos_name)
        _agentos_dst = os.path.join(__agentos_vfs_site, _agentos_name)
        if os.path.isdir(_agentos_src):
            shutil.copytree(_agentos_src, _agentos_dst, dirs_exist_ok=True)
        else:
            shutil.copy2(_agentos_src, _agentos_dst)
    print("Successfully installed " + " ".join(_agentos_pip_pkgs))
elif _agentos_pip_args and _agentos_pip_args[0] in ("--version", "-V", "version"):
    print("pip (agentOS micropip shim)")
elif _agentos_pip_args and _agentos_pip_args[0] == "list":
    import micropip
    for _agentos_pkg in sorted(micropip.list()):
        print(_agentos_pkg)
else:
    print("usage: pip install <package> [<package> ...]", file=sys.stderr)
    sys.exit(2)
`);
  } finally {
    try {
      pyodide.globals.delete('__agentos_vfs_site');
    } catch (error) {
      writeStream(
        process.stderr,
        `agentos: pip VFS global cleanup failed: ${formatError(error)}\n`,
      );
    }
  }
}

// Minimal interactive REPL backed by the kernel stdin stream (sys.stdin via
// setStdin). Prompts use the standard PS1/PS2; EOF on stdin ends the session.
async function runPythonRepl(pyodide) {
  await pyodide.runPythonAsync(`
import sys
from code import InteractiveConsole
if not hasattr(sys, "ps1"):
    sys.ps1 = ">>> "
if not hasattr(sys, "ps2"):
    sys.ps2 = "... "
InteractiveConsole(locals={"__name__": "__main__", "__doc__": None}).interact(banner="", exitmsg="")
`);
}

function resolvePythonSource(pyodide) {
  const filePath = readRunnerEnv(PYTHON_FILE_ENV);
  if (filePath != null) {
    if (typeof pyodide?.FS?.readFile !== 'function') {
      throw new Error(`Pyodide FS.readFile() is required to execute ${filePath}`);
    }

    return pyodide.FS.readFile(filePath, { encoding: 'utf8' });
  }

  return requiredEnv(PYTHON_CODE_ENV);
}

let pythonVfsRpcBridge = null;
let retainLanguageContext = false;

try {
  const startupStarted = realPerformance.now();
  emitWarmupStage('startup');
  emitWarmupStage(`python-rpc-bridge:${bridgePythonRpc ? 'on' : 'off'}`);
  const { indexPath, indexUrl } = resolveIndexLocation(requiredEnv(PYODIDE_INDEX_URL_ENV));
  const bundledPackageBaseUrl = normalizeBaseUrl(indexPath);
  const packageBaseUrl = normalizeBaseUrl(
    readRunnerEnv(PYODIDE_PACKAGE_BASE_URL_ENV) ?? bundledPackageBaseUrl,
  );
  const packageCacheDir = resolvePyodidePackageCacheDir();
  emitWarmupStage(`package-cache-dir:${packageCacheDir}`);
  const prewarmOnly = readRunnerEnv(PYTHON_PREWARM_ONLY_ENV) === '1';
  retainLanguageContext = readRunnerEnv(RETAIN_LANGUAGE_CONTEXT_ENV) === '1';
  const preloadPackages = parsePreloadPackages(readRunnerEnv(PYTHON_PRELOAD_PACKAGES_ENV));
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
  if (retainLanguageContext) {
    globalThis.__agentOSRetainedPyodide = pyodide;
  }
  restorePyodideShellCompat();
  emitWarmupStage('after-load-pyodide');
  const loadPyodideMs = realPerformance.now() - loadPyodideStarted;
  let packageLoadMs = 0;

  installPythonStdin(pyodide);
  installPythonWorkspaceFs(pyodide, pythonVfsRpcBridge);
  installPythonVfsSitePackages(pyodide);
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
  applyPythonArgv(pyodide);
  const moduleName = readRunnerEnv(PYTHON_MODULE_ENV);
  const stdinProgram = readRunnerEnv(PYTHON_STDIN_PROGRAM_ENV) === '1';
  const interactive = readRunnerEnv(PYTHON_INTERACTIVE_ENV) === '1';
  const source = moduleName
    ? `module:${moduleName}`
    : stdinProgram
      ? 'stdin'
      : interactive
        ? 'repl'
        : readRunnerEnv(PYTHON_FILE_ENV) != null
          ? 'file'
          : 'inline';
  emitPythonStartupMetrics({
    prewarmOnly: false,
    startupMs: realPerformance.now() - startupStarted,
    loadPyodideMs,
    packageLoadMs,
    packageCount: preloadPackages.length,
    source,
  });
  if (moduleName === 'pip') {
    await runPythonPip(pyodide);
  } else if (moduleName) {
    pyodide.globals.set('__agentos_module', moduleName);
    try {
      await pyodide.runPythonAsync(
        'import runpy\nrunpy.run_module(__agentos_module, run_name="__main__", alter_sys=True)',
      );
    } finally {
      pyodide.globals.delete('__agentos_module');
    }
  } else if (stdinProgram) {
    await pyodide.runPythonAsync(readProgramFromStdin());
  } else if (interactive) {
    await runPythonRepl(pyodide);
  } else {
    await pyodide.runPythonAsync(resolvePythonSource(pyodide));
  }
  }
} catch (error) {
  writeStream(process.stderr, formatError(error));
  process.exitCode = 1;
} finally {
  if (!retainLanguageContext) {
    pythonVfsRpcBridge?.dispose();
  }
  emitControlMessage({ type: 'python_exit', exitCode: process.exitCode ?? 0 });
}
if (!retainLanguageContext) {
  process.exit(process.exitCode ?? 0);
}
