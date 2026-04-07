import { closeSync, createReadStream, readSync, writeSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { register } from 'node:module';
import { performance as realPerformance } from 'node:perf_hooks';
import path from 'node:path';
import readline from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ACCESS_DENIED_CODE = 'ERR_ACCESS_DENIED';
const ASSET_ROOT_ENV = 'AGENT_OS_NODE_IMPORT_CACHE_ASSET_ROOT';
const PYODIDE_INDEX_URL_ENV = 'AGENT_OS_PYODIDE_INDEX_URL';
const PYTHON_CODE_ENV = 'AGENT_OS_PYTHON_CODE';
const PYTHON_FILE_ENV = 'AGENT_OS_PYTHON_FILE';
const PYTHON_PREWARM_ONLY_ENV = 'AGENT_OS_PYTHON_PREWARM_ONLY';
const PYTHON_WARMUP_DEBUG_ENV = 'AGENT_OS_PYTHON_WARMUP_DEBUG';
const PYTHON_WARMUP_METRICS_PREFIX = '__AGENT_OS_PYTHON_WARMUP_METRICS__:';
const PYTHON_PRELOAD_PACKAGES_ENV = 'AGENT_OS_PYTHON_PRELOAD_PACKAGES';
const PYTHON_VFS_RPC_REQUEST_FD_ENV = 'AGENT_OS_PYTHON_VFS_RPC_REQUEST_FD';
const PYTHON_VFS_RPC_RESPONSE_FD_ENV = 'AGENT_OS_PYTHON_VFS_RPC_RESPONSE_FD';
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
const originalGetBuiltinModule =
  typeof process.getBuiltinModule === 'function'
    ? process.getBuiltinModule.bind(process)
    : null;
const CONTROL_PIPE_FD = parseControlPipeFd(process.env.AGENT_OS_CONTROL_PIPE_FD);

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

function resolveIndexLocation(value) {
  if (/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value)) {
    const normalizedUrl = value.endsWith('/') ? value : `${value}/`;
    if (!normalizedUrl.startsWith('file:')) {
      return {
        indexPath: normalizedUrl,
        indexUrl: normalizedUrl,
      };
    }

    const indexPath = normalizeDirectoryPath(fileURLToPath(normalizedUrl));
    return {
      indexPath,
      indexUrl: pathToFileURL(indexPath).href,
    };
  }

  const indexPath = normalizeDirectoryPath(path.resolve(value));
  return {
    indexPath,
    indexUrl: pathToFileURL(indexPath).href,
  };
}

function writeStream(stream, message) {
  if (message == null) {
    return;
  }

  const value = typeof message === 'string' ? message : String(message);
  stream.write(value.endsWith('\n') ? value : `${value}\n`);
}

function formatError(error) {
  if (error instanceof Error) {
    return error.stack || error.message || String(error);
  }

  return String(error);
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

function rejectPendingRpcRequests(pending, error) {
  for (const { reject } of pending.values()) {
    reject(error);
  }
  pending.clear();
}

function createPythonVfsRpcBridge() {
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

_agent_os_original_import = _agent_os_builtins.__import__

def _agent_os_import(name, globals=None, locals=None, fromlist=(), level=0):
    if name in _agent_os_blocked_modules:
        return _agent_os_blocked_modules[name]
    return _agent_os_original_import(name, globals, locals, fromlist, level)

_agent_os_builtins.__import__ = _agent_os_import
_agent_os_sys.modules.update(_agent_os_blocked_modules)
`;

function hardenProperty(target, key, value) {
  try {
    Object.defineProperty(target, key, {
      value,
      writable: false,
      configurable: false,
    });
  } catch (error) {
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

function installPythonGuestPreloadHardening() {
  if (originalRequire) {
    hardenProperty(globalThis, 'require', () => {
      throw accessDenied('require');
    });
  }

  if (originalFetch) {
    const restrictedFetch = (resource, init) => {
      const candidate =
        typeof resource === 'string'
          ? resource
          : resource instanceof URL
            ? resource.href
            : resource?.url;

      let url;
      try {
        url = new URL(String(candidate ?? ''));
      } catch {
        throw accessDenied('network access');
      }

      if (url.protocol !== 'data:') {
        throw accessDenied(`network access to ${url.protocol}`);
      }

      return originalFetch(resource, init);
    };

    hardenProperty(globalThis, 'fetch', restrictedFetch);
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
  if (!assetRoot) {
    return;
  }

  register(new URL('./loader.mjs', import.meta.url), import.meta.url);
}

function installPythonVfsRpcBridge() {
  const bridge = createPythonVfsRpcBridge();
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

    const message = String(error?.message || error);
    let errno = ERRNO_CODES.EIO;
    if (/permission denied|access denied|denied/i.test(message)) {
      errno = ERRNO_CODES.EACCES;
    } else if (/read-only|erofs/i.test(message)) {
      errno = ERRNO_CODES.EROFS;
    } else if (/not a directory|enotdir/i.test(message)) {
      errno = ERRNO_CODES.ENOTDIR;
    } else if (/is a directory|eisdir/i.test(message)) {
      errno = ERRNO_CODES.EISDIR;
    } else if (/exists|already exists|eexist/i.test(message)) {
      errno = ERRNO_CODES.EEXIST;
    } else if (/not found|no such file|enoent/i.test(message)) {
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

async function readLockFileContents(indexURL) {
  const lockFileUrl = new URL('pyodide-lock.json', indexURL);
  return readFile(lockFileUrl, 'utf8');
}

function installPythonStdin(pyodide) {
  if (typeof pyodide?.setStdin !== 'function') {
    return;
  }

  pyodide.setStdin({
    isatty: false,
    read(buffer) {
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
  const { indexPath, indexUrl } = resolveIndexLocation(requiredEnv(PYODIDE_INDEX_URL_ENV));
  const prewarmOnly = process.env[PYTHON_PREWARM_ONLY_ENV] === '1';
  const preloadPackages = parsePreloadPackages(process.env[PYTHON_PRELOAD_PACKAGES_ENV]);
  const lockFileContents = await readLockFileContents(indexUrl);
  const pyodideModuleUrl = new URL('pyodide.mjs', indexUrl).href;
  const { loadPyodide } = await import(pyodideModuleUrl);

  if (typeof loadPyodide !== 'function') {
    throw new Error(`pyodide.mjs at ${indexUrl} does not export loadPyodide()`);
  }

  installPythonGuestPreloadHardening();
  const loadPyodideStarted = realPerformance.now();
  const pyodide = await loadPyodide({
    indexURL: indexPath,
    lockFileContents,
    packageBaseUrl: indexPath,
    stdout: (message) => writeStream(process.stdout, message),
    stderr: (message) => writeStream(process.stderr, message),
  });
  const loadPyodideMs = realPerformance.now() - loadPyodideStarted;
  let packageLoadMs = 0;

  if (prewarmOnly) {
    emitPythonStartupMetrics({
      prewarmOnly: true,
      startupMs: realPerformance.now() - startupStarted,
      loadPyodideMs,
      packageLoadMs,
      packageCount: 0,
      source: 'prewarm',
    });
    process.exitCode = 0;
  } else {
  installPythonStdin(pyodide);
  pythonVfsRpcBridge = installPythonVfsRpcBridge();
  installPythonWorkspaceFs(pyodide, pythonVfsRpcBridge);
  installPythonGuestLoaderHooks();
  if (preloadPackages.length > 0) {
    const packageLoadStarted = realPerformance.now();
    await pyodide.loadPackage(preloadPackages);
    packageLoadMs = realPerformance.now() - packageLoadStarted;
  }
  installPythonGuestProcessHardening();
  installPythonGuestImportBlocklist(pyodide);
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
