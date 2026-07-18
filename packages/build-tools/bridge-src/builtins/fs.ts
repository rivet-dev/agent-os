import { deferCloseIfChildInheritedFd } from "./child-process.js";
import { builtinCryptoModule } from "./crypto.js";
import { _umask } from "./process.js";
import { clearTimeout2, setTimeout2 } from "./timers.js";
import { exposeCustomGlobal } from "../global-exposure.js";
import { require_buffer } from "../vendor/buffer.js";
import { __toESM } from "../vendor/esbuild-runtime.js";

var import_buffer = __toESM(require_buffer(), 1);
var O_RDONLY = 0;
var O_WRONLY = 1;
var O_RDWR = 2;
var O_CREAT = 64;
var O_EXCL = 128;
var O_TRUNC = 512;
var O_APPEND = 1024;
var KERNEL_POLLIN = 1;
var Stats = class {
  dev;
  ino;
  mode;
  nlink;
  uid;
  gid;
  rdev;
  size;
  blksize;
  blocks;
  atimeMs;
  mtimeMs;
  ctimeMs;
  birthtimeMs;
  atime;
  mtime;
  ctime;
  birthtime;
  constructor(init) {
    this.dev = init.dev ?? 0;
    this.ino = init.ino ?? 0;
    this.mode = init.mode;
    this.nlink = init.nlink ?? 1;
    this.uid = init.uid ?? 0;
    this.gid = init.gid ?? 0;
    this.rdev = init.rdev ?? 0;
    this.size = init.size;
    this.blksize = init.blksize ?? 4096;
    this.blocks = init.blocks ?? Math.ceil(init.size / 512);
    const atimeMs = init.atimeMs ?? Date.now();
    const mtimeMs = init.mtimeMs ?? Date.now();
    const ctimeMs = init.ctimeMs ?? Date.now();
    this.atimeMs = atimeMs + ((init.atimeNsec ?? 0) % 1e6) / 1e6;
    this.mtimeMs = mtimeMs + ((init.mtimeNsec ?? 0) % 1e6) / 1e6;
    this.ctimeMs = ctimeMs + ((init.ctimeNsec ?? 0) % 1e6) / 1e6;
    this.birthtimeMs = init.birthtimeMs ?? Date.now();
    this.atime = new Date(this.atimeMs);
    this.mtime = new Date(this.mtimeMs);
    this.ctime = new Date(this.ctimeMs);
    this.birthtime = new Date(this.birthtimeMs);
  }
  isFile() {
    return (this.mode & 61440) === 32768;
  }
  isDirectory() {
    return (this.mode & 61440) === 16384;
  }
  isSymbolicLink() {
    return (this.mode & 61440) === 40960;
  }
  isBlockDevice() {
    return (this.mode & 61440) === 24576;
  }
  isCharacterDevice() {
    return (this.mode & 61440) === 8192;
  }
  isFIFO() {
    return (this.mode & 61440) === 4096;
  }
  isSocket() {
    return (this.mode & 61440) === 49152;
  }
};
var Dirent = class {
  name;
  parentPath;
  path;
  // Deprecated alias for parentPath
  _isDir;
  constructor(name, isDir, parentPath = "") {
    this.name = name;
    this._isDir = isDir;
    this.parentPath = parentPath;
    this.path = parentPath;
  }
  isFile() {
    return !this._isDir;
  }
  isDirectory() {
    return this._isDir;
  }
  isSymbolicLink() {
    return false;
  }
  isBlockDevice() {
    return false;
  }
  isCharacterDevice() {
    return false;
  }
  isFIFO() {
    return false;
  }
  isSocket() {
    return false;
  }
};
var Dir = class {
  path;
  _entries = null;
  _index = 0;
  _closed = false;
  constructor(dirPath) {
    this.path = dirPath;
  }
  _load() {
    if (this._entries === null) {
      this._entries = fs.readdirSync(this.path, { withFileTypes: true });
    }
    return this._entries;
  }
  readSync() {
    if (this._closed) throw new Error("Directory handle was closed");
    const entries = this._load();
    if (this._index >= entries.length) return null;
    return entries[this._index++];
  }
  async read() {
    return this.readSync();
  }
  closeSync() {
    this._closed = true;
  }
  async close() {
    this.closeSync();
  }
  async *[Symbol.asyncIterator]() {
    const entries = this._load();
    for (const entry of entries) {
      if (this._closed) return;
      yield entry;
    }
    this._closed = true;
  }
};
var FILE_HANDLE_READ_CHUNK_BYTES = 64 * 1024;
var FILE_HANDLE_READ_BUFFER_BYTES = 16 * 1024;
var FILE_HANDLE_MAX_READ_BYTES = 2 ** 31 - 1;
var READ_FILE_SYNC_CHUNK_BYTES = 8 * 1024 * 1024;
// Raw bridge responses include a small serialization envelope and must stay
// below the sidecar frame cap. Node callers already handle short reads.
var MAX_SYNC_BRIDGE_READ_BYTES = 8 * 1024 * 1024;
function createAbortError(reason) {
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  if (reason !== void 0) {
    error.cause = reason;
  }
  return error;
}
function validateAbortSignal(signal) {
  if (signal === void 0) {
    return void 0;
  }
  if (signal === null || typeof signal !== "object" || typeof signal.aborted !== "boolean" || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function") {
    const error = new TypeError(
      'The "signal" argument must be an instance of AbortSignal'
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }
  return signal;
}
function throwIfAborted(signal) {
  if (signal?.aborted) {
    throw createAbortError(signal.reason);
  }
}
function waitForNextTick() {
  return new Promise((resolve) => process.nextTick(resolve));
}
function createInternalAssertionError(message) {
  const error = new Error(message);
  error.code = "ERR_INTERNAL_ASSERTION";
  return error;
}
function createOutOfRangeError(name, range, received) {
  const error = new RangeError(
    `The value of "${name}" is out of range. It must be ${range}. Received ${String(received)}`
  );
  error.code = "ERR_OUT_OF_RANGE";
  return error;
}
function formatInvalidArgReceived(actual) {
  if (actual === null) {
    return "Received null";
  }
  if (actual === void 0) {
    return "Received undefined";
  }
  if (typeof actual === "string") {
    return `Received type string ('${actual}')`;
  }
  if (typeof actual === "number") {
    return `Received type number (${String(actual)})`;
  }
  if (typeof actual === "boolean") {
    return `Received type boolean (${String(actual)})`;
  }
  if (typeof actual === "bigint") {
    return `Received type bigint (${actual.toString()}n)`;
  }
  if (typeof actual === "symbol") {
    return `Received type symbol (${String(actual)})`;
  }
  if (typeof actual === "function") {
    return actual.name ? `Received function ${actual.name}` : "Received function";
  }
  if (Array.isArray(actual)) {
    return "Received an instance of Array";
  }
  if (actual && typeof actual === "object") {
    const constructorName = actual.constructor?.name;
    if (constructorName) {
      return `Received an instance of ${constructorName}`;
    }
  }
  return `Received type ${typeof actual} (${String(actual)})`;
}
function createInvalidArgTypeError(name, expected, actual) {
  const error = new TypeError(
    `The "${name}" argument must be ${expected}. ${formatInvalidArgReceived(actual)}`
  );
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
}
function createInvalidArgValueError(name, message) {
  const error = new TypeError(
    `The argument '${name}' ${message}`
  );
  error.code = "ERR_INVALID_ARG_VALUE";
  return error;
}
function createInvalidEncodingError(encoding) {
  const printable = typeof encoding === "string" ? `'${encoding}'` : encoding === void 0 ? "undefined" : encoding === null ? "null" : String(encoding);
  const error = new TypeError(
    `The argument 'encoding' is invalid encoding. Received ${printable}`
  );
  error.code = "ERR_INVALID_ARG_VALUE";
  return error;
}
function toUint8ArrayChunk(chunk, encoding) {
  if (typeof chunk === "string") {
    return import_buffer.Buffer.from(chunk, encoding ?? "utf8");
  }
  if (import_buffer.Buffer.isBuffer(chunk)) {
    return new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
  }
  if (chunk instanceof Uint8Array) {
    return chunk;
  }
  if (ArrayBuffer.isView(chunk)) {
    return new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
  }
  throw createInvalidArgTypeError("data", "a string, Buffer, TypedArray, or DataView", chunk);
}
async function* iterateWriteChunks(data, encoding) {
  if (typeof data === "string" || ArrayBuffer.isView(data)) {
    yield toUint8ArrayChunk(data, encoding);
    return;
  }
  if (data && typeof data[Symbol.asyncIterator] === "function") {
    for await (const chunk of data) {
      yield toUint8ArrayChunk(chunk, encoding);
    }
    return;
  }
  if (data && typeof data[Symbol.iterator] === "function") {
    for (const chunk of data) {
      yield toUint8ArrayChunk(chunk, encoding);
    }
    return;
  }
  throw createInvalidArgTypeError("data", "a string, Buffer, TypedArray, DataView, or Iterable", data);
}
var FileHandle = class _FileHandle {
  _fd;
  _closing = false;
  _closed = false;
  _listeners = /* @__PURE__ */ new Map();
  constructor(fd) {
    this._fd = fd;
  }
  static _assertHandle(handle) {
    if (!(handle instanceof _FileHandle)) {
      throw createInternalAssertionError("handle must be an instance of FileHandle");
    }
    return handle;
  }
  _emitCloseOnce() {
    if (this._closed) {
      this._fd = -1;
      this.emit("close");
      return;
    }
    this._closed = true;
    this._fd = -1;
    this.emit("close");
  }
  _resolvePath() {
    if (this._fd < 0) {
      return null;
    }
    return _fdGetPath.applySync(void 0, [this._fd]);
  }
  get fd() {
    return this._fd;
  }
  get closed() {
    return this._closed;
  }
  on(event, listener) {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    wrapper._originalListener = listener;
    return this.on(event, wrapper);
  }
  off(event, listener) {
    const listeners = this._listeners.get(event);
    if (!listeners) {
      return this;
    }
    const index = listeners.findIndex(
      (candidate) => candidate === listener || candidate._originalListener === listener
    );
    if (index !== -1) {
      listeners.splice(index, 1);
    }
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  emit(event, ...args) {
    const listeners = this._listeners.get(event);
    if (!listeners || listeners.length === 0) {
      return false;
    }
    for (const listener of listeners.slice()) {
      listener(...args);
    }
    return true;
  }
  async close() {
    const handle = _FileHandle._assertHandle(this);
    if (handle._closing || handle._closed) {
      if (handle._fd < 0) {
        throw createFsError("EBADF", "EBADF: bad file descriptor, close", "close");
      }
    }
    handle._closing = true;
    try {
      fs.closeSync(handle._fd);
      handle._emitCloseOnce();
    } finally {
      handle._closing = false;
    }
  }
  async [Symbol.asyncDispose]() {
    if (!this._closed) {
      await this.close();
    }
  }
  async stat() {
    const handle = _FileHandle._assertHandle(this);
    return fs.fstatSync(handle.fd);
  }
  async sync() {
    const handle = _FileHandle._assertHandle(this);
    fs.fsyncSync(handle.fd);
  }
  async datasync() {
    return this.sync();
  }
  async truncate(len) {
    const handle = _FileHandle._assertHandle(this);
    fs.ftruncateSync(handle.fd, len);
  }
  async chmod(mode) {
    const handle = _FileHandle._assertHandle(this);
    const path = handle._resolvePath();
    if (!path) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chmod");
    }
    fs.chmodSync(path, mode);
  }
  async chown(uid, gid) {
    const handle = _FileHandle._assertHandle(this);
    const path = handle._resolvePath();
    if (!path) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chown");
    }
    fs.chownSync(path, uid, gid);
  }
  async utimes(atime, mtime) {
    const handle = _FileHandle._assertHandle(this);
    fs.futimesSync(handle.fd, atime, mtime);
  }
  async read(buffer, offset, length, position) {
    const handle = _FileHandle._assertHandle(this);
    let target = buffer;
    let readOffset = offset;
    let readLength = length;
    let readPosition = position;
    if (target !== null && typeof target === "object" && !ArrayBuffer.isView(target)) {
      readOffset = target.offset;
      readLength = target.length;
      readPosition = target.position;
      target = target.buffer ?? null;
    }
    if (target === null) {
      target = import_buffer.Buffer.alloc(FILE_HANDLE_READ_BUFFER_BYTES);
    }
    if (!ArrayBuffer.isView(target)) {
      throw createInvalidArgTypeError("buffer", "an instance of ArrayBufferView", target);
    }
    const normalizedOffset = readOffset ?? 0;
    const normalizedLength = readLength ?? target.byteLength - normalizedOffset;
    const bytesRead = fs.readSync(
      handle.fd,
      target,
      normalizedOffset,
      normalizedLength,
      readPosition ?? null
    );
    return { bytesRead, buffer: target };
  }
  async write(buffer, offsetOrPosition, lengthOrEncoding, position) {
    const handle = _FileHandle._assertHandle(this);
    if (typeof buffer === "string") {
      const encoding = typeof lengthOrEncoding === "string" ? lengthOrEncoding : "utf8";
      if (encoding === "hex" && buffer.length % 2 !== 0) {
        throw createInvalidArgValueError("encoding", `is invalid for data of length ${buffer.length}`);
      }
      const bytesWritten2 = fs.writeSync(handle.fd, import_buffer.Buffer.from(buffer, encoding), 0, void 0, offsetOrPosition ?? null);
      return { bytesWritten: bytesWritten2, buffer };
    }
    if (!ArrayBuffer.isView(buffer)) {
      throw createInvalidArgTypeError("buffer", "a string, Buffer, TypedArray, or DataView", buffer);
    }
    const offset = offsetOrPosition ?? 0;
    const length = typeof lengthOrEncoding === "number" ? lengthOrEncoding : void 0;
    const bytesWritten = fs.writeSync(handle.fd, buffer, offset, length, position ?? null);
    return { bytesWritten, buffer };
  }
  async readFile(options) {
    const handle = _FileHandle._assertHandle(this);
    const normalized = typeof options === "string" ? { encoding: options } : options ?? void 0;
    const signal = validateAbortSignal(normalized?.signal);
    const encoding = normalized?.encoding ?? void 0;
    const stats = await handle.stat();
    if (stats.size > FILE_HANDLE_MAX_READ_BYTES) {
      const error = new RangeError("File size is greater than 2 GiB");
      error.code = "ERR_FS_FILE_TOO_LARGE";
      throw error;
    }
    await waitForNextTick();
    throwIfAborted(signal);
    const chunks = [];
    let totalLength = 0;
    while (true) {
      throwIfAborted(signal);
      const chunk = import_buffer.Buffer.alloc(FILE_HANDLE_READ_CHUNK_BYTES);
      const { bytesRead } = await handle.read(chunk, 0, chunk.byteLength, null);
      if (bytesRead === 0) {
        break;
      }
      chunks.push(chunk.subarray(0, bytesRead));
      totalLength += bytesRead;
      if (totalLength > FILE_HANDLE_MAX_READ_BYTES) {
        const error = new RangeError("File size is greater than 2 GiB");
        error.code = "ERR_FS_FILE_TOO_LARGE";
        throw error;
      }
      await waitForNextTick();
    }
    const result = import_buffer.Buffer.concat(chunks, totalLength);
    return encoding ? result.toString(encoding) : result;
  }
  async writeFile(data, options) {
    const handle = _FileHandle._assertHandle(this);
    const normalized = typeof options === "string" ? { encoding: options } : options ?? void 0;
    const signal = validateAbortSignal(normalized?.signal);
    const encoding = normalized?.encoding ?? void 0;
    await waitForNextTick();
    throwIfAborted(signal);
    for await (const chunk of iterateWriteChunks(data, encoding)) {
      throwIfAborted(signal);
      await handle.write(chunk, 0, chunk.byteLength, void 0);
      await waitForNextTick();
    }
  }
  async appendFile(data, options) {
    return this.writeFile(data, options);
  }
  createReadStream(options) {
    _FileHandle._assertHandle(this);
    return new ReadStream(null, { ...options ?? {}, fd: this });
  }
  createWriteStream(options) {
    _FileHandle._assertHandle(this);
    return new WriteStream(null, { ...options ?? {}, fd: this });
  }
};
function isArrayBufferView(value) {
  return ArrayBuffer.isView(value);
}
function createInvalidPropertyTypeError(propertyPath, actual) {
  let received;
  if (actual === null) {
    received = "Received null";
  } else if (typeof actual === "string") {
    received = `Received type string ('${actual}')`;
  } else {
    received = `Received type ${typeof actual} (${String(actual)})`;
  }
  const error = new TypeError(
    `The "${propertyPath}" property must be of type function. ${received}`
  );
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
}
function validateCallback(callback, name = "cb") {
  if (typeof callback !== "function") {
    throw createInvalidArgTypeError(name, "of type function", callback);
  }
}
function validateEncodingValue(encoding) {
  if (encoding === void 0 || encoding === null) {
    return;
  }
  if (typeof encoding !== "string" || !import_buffer.Buffer.isEncoding(encoding)) {
    throw createInvalidEncodingError(encoding);
  }
}
function validateEncodingOption(options) {
  if (typeof options === "string") {
    validateEncodingValue(options);
    return;
  }
  if (options && typeof options === "object" && "encoding" in options) {
    validateEncodingValue(options.encoding);
  }
}
function normalizePathLike(path, name = "path") {
  if (typeof path === "string") {
    return path;
  }
  if (import_buffer.Buffer.isBuffer(path)) {
    return path.toString("utf8");
  }
  if (path instanceof URL) {
    if (path.protocol === "file:") {
      return path.pathname;
    }
    throw createInvalidArgTypeError(name, "of type string or an instance of Buffer or URL", path);
  }
  throw createInvalidArgTypeError(name, "of type string or an instance of Buffer or URL", path);
}
function resolveOperationPath(path) {
  const normalized = normalizePathLike(path);
  if (normalized.startsWith("/")) {
    return normalized;
  }
  const cwd = typeof globalThis.process?.cwd === "function"
    ? globalThis.process.cwd()
    : typeof _processConfig !== "undefined" && typeof _processConfig.cwd === "string"
      ? _processConfig.cwd
      : "/";
  return `${cwd.replace(/\/$/, "")}/${normalized}`;
}
function tryNormalizeExistsPath(path) {
  try {
    return normalizePathLike(path);
  } catch {
    return null;
  }
}
function normalizeNumberArgument(name, value, options = {}) {
  const { min = 0, max = 2147483647, allowNegativeOne = false } = options;
  if (typeof value !== "number") {
    throw createInvalidArgTypeError(name, "of type number", value);
  }
  if (!Number.isFinite(value) || !Number.isInteger(value)) {
    throw createOutOfRangeError(name, "an integer", value);
  }
  if (allowNegativeOne && value === -1 || value >= min && value <= max) {
    return value;
  }
  throw createOutOfRangeError(name, `>= ${min} && <= ${max}`, value);
}
function normalizeModeArgument(mode, name = "mode") {
  if (typeof mode === "string") {
    if (!/^[0-7]+$/.test(mode)) {
      throw createInvalidArgValueError(name, "must be a 32-bit unsigned integer or an octal string. Received '" + mode + "'");
    }
    return parseInt(mode, 8);
  }
  return normalizeNumberArgument(name, mode, { min: 0, max: 4294967295 });
}
function normalizeOpenModeArgument(mode) {
  if (mode === void 0 || mode === null) {
    return void 0;
  }
  return normalizeModeArgument(mode);
}
function applyProcessUmask(mode) {
  return (mode & ~0o777) | ((mode & 0o777) & ~(_umask & 0o777));
}
function validateWriteStreamStartOption(options) {
  if (options?.start === void 0) {
    return;
  }
  if (typeof options.start !== "number") {
    throw createInvalidArgTypeError("start", "of type number", options.start);
  }
  if (!Number.isFinite(options.start) || !Number.isInteger(options.start) || options.start < 0) {
    throw createOutOfRangeError("start", ">= 0", options.start);
  }
}
function validateBooleanOption(name, value) {
  if (value === void 0) {
    return void 0;
  }
  if (typeof value !== "boolean") {
    throw createInvalidArgTypeError(name, "of type boolean", value);
  }
  return value;
}
function validateAbortSignalOption(name, signal) {
  if (signal === void 0) {
    return void 0;
  }
  if (signal === null || typeof signal !== "object" || typeof signal.aborted !== "boolean" || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function") {
    const error = new TypeError(
      `The "${name}" property must be an instance of AbortSignal. ${formatInvalidArgReceived(signal)}`
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }
  return signal;
}
function normalizeWatchOptions(options, allowString) {
  let normalized;
  if (options === void 0 || options === null) {
    normalized = {};
  } else if (typeof options === "string") {
    if (!allowString) {
      throw createInvalidArgTypeError("options", "of type object", options);
    }
    validateEncodingValue(options);
    normalized = { encoding: options };
  } else if (typeof options === "object") {
    normalized = options;
  } else {
    throw createInvalidArgTypeError(
      "options",
      allowString ? "one of type string or object" : "of type object",
      options
    );
  }
  validateBooleanOption("options.persistent", normalized.persistent);
  validateBooleanOption("options.recursive", normalized.recursive);
  validateEncodingOption(normalized);
  const signal = validateAbortSignalOption("options.signal", normalized.signal);
  return {
    persistent: normalized.persistent,
    recursive: normalized.recursive,
    encoding: normalized.encoding,
    signal
  };
}
function normalizeWatchArguments(path, optionsOrListener, listener) {
  const pathStr = normalizePathLike(path);
  let options = optionsOrListener;
  let resolvedListener = listener;
  if (typeof optionsOrListener === "function") {
    options = void 0;
    resolvedListener = optionsOrListener;
  }
  if (resolvedListener !== void 0 && typeof resolvedListener !== "function") {
    throw createInvalidArgTypeError("listener", "of type function", resolvedListener);
  }
  return {
    path: pathStr,
    listener: resolvedListener,
    options: normalizeWatchOptions(options, true)
  };
}
function normalizeWatchFileArguments(path, optionsOrListener, listener) {
  const pathStr = normalizePathLike(path);
  let options = {};
  let resolvedListener = listener;
  if (typeof optionsOrListener === "function") {
    resolvedListener = optionsOrListener;
  } else if (optionsOrListener === void 0 || optionsOrListener === null) {
    options = {};
  } else if (typeof optionsOrListener === "object") {
    options = optionsOrListener;
  } else {
    throw createInvalidArgTypeError("listener", "of type function", optionsOrListener);
  }
  if (typeof resolvedListener !== "function") {
    throw createInvalidArgTypeError("listener", "of type function", resolvedListener);
  }
  validateBooleanOption("persistent", options.persistent);
  validateBooleanOption("bigint", options.bigint);
  if (options.interval !== void 0 && typeof options.interval !== "number") {
    throw createInvalidArgTypeError("interval", "of type number", options.interval);
  }
  return {
    path: pathStr,
    listener: resolvedListener,
    options: {
      persistent: options.persistent,
      bigint: options.bigint,
      interval: options.interval
    }
  };
}
function createMissingWatcherStats() {
  return new Stats({
    mode: 0,
    size: 0,
    dev: 0,
    ino: 0,
    nlink: 0,
    uid: 0,
    gid: 0,
    rdev: 0,
    blksize: 0,
    blocks: 0,
    atimeMs: 0,
    mtimeMs: 0,
    ctimeMs: 0,
    birthtimeMs: 0
  });
}
function createWatcherSnapshot(path) {
  try {
    const stats = fs.statSync(path);
    return {
      exists: true,
      stats,
      signature: JSON.stringify({
        dev: stats.dev,
        ino: stats.ino,
        mode: stats.mode,
        nlink: stats.nlink,
        uid: stats.uid,
        gid: stats.gid,
        rdev: stats.rdev,
        size: stats.size,
        atimeMs: stats.atimeMs,
        mtimeMs: stats.mtimeMs,
        ctimeMs: stats.ctimeMs,
        birthtimeMs: stats.birthtimeMs
      })
    };
  } catch (error) {
    if (error?.code === "ENOENT" || error?.code === "ENOTDIR") {
      return {
        exists: false,
        stats: createMissingWatcherStats(),
        signature: "missing"
      };
    }
    throw error;
  }
}
function createWatcherFilename(path, encoding) {
  const basename = path === "/" ? "" : path.split("/").filter(Boolean).pop() ?? "";
  if (encoding === "buffer") {
    return import_buffer.Buffer.from(basename);
  }
  return basename;
}
function watcherEventType(previous, current) {
  if (previous.exists !== current.exists) {
    return "rename";
  }
  return "change";
}
var DEFAULT_FS_WATCH_INTERVAL_MS = 50;
var MAX_IDLE_FS_WATCH_INTERVAL_MS = 1e3;
var DEFAULT_FS_WATCH_FILE_INTERVAL_MS = 5007;
var activeStatWatchers = /* @__PURE__ */ new Map();
var PollingFsWatcher = class {
  constructor(path, options) {
    this._path = path;
    this._intervalMs = options.interval;
    this._maxIntervalMs = Math.max(options.interval, options.maxInterval ?? options.interval);
    this._nextIntervalMs = this._intervalMs;
    this._onChange = options.onChange;
    this._onClose = options.onClose;
    this._listeners = /* @__PURE__ */ new Map();
    this._closed = false;
    this._persistent = options.persistent !== false;
    this._signal = options.signal;
    this._snapshot = createWatcherSnapshot(path);
    this._schedulePoll = () => {
      if (this._closed) {
        return;
      }
      this._timer = setTimeout2(this._poll, this._nextIntervalMs);
      if (!this._persistent) {
        this._timer?.unref?.();
      }
    };
    this._poll = () => {
      if (this._closed) {
        return;
      }
      let next;
      try {
        next = createWatcherSnapshot(this._path);
      } catch (error) {
        this._nextIntervalMs = Math.min(this._nextIntervalMs * 2, this._maxIntervalMs);
        this._schedulePoll();
        this.emit("error", error);
        return;
      }
      if (next.signature === this._snapshot.signature) {
        this._nextIntervalMs = Math.min(this._nextIntervalMs * 2, this._maxIntervalMs);
        this._schedulePoll();
        return;
      }
      const previous = this._snapshot;
      this._snapshot = next;
      this._nextIntervalMs = this._intervalMs;
      this._schedulePoll();
      this._onChange(next, previous);
    };
    this._handleAbort = () => {
      this.close();
    };
    this._schedulePoll();
    if (this._signal) {
      if (this._signal.aborted) {
        queueMicrotask(() => this.close());
      } else {
        this._signal.addEventListener("abort", this._handleAbort, { once: true });
      }
    }
  }
  _path;
  _intervalMs;
  _maxIntervalMs;
  _nextIntervalMs;
  _onChange;
  _onClose;
  _listeners;
  _timer;
  _closed;
  _persistent;
  _signal;
  _handleAbort;
  _snapshot;
  _schedulePoll;
  _poll;
  on(event, listener) {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    return this;
  }
  addListener(event, listener) {
    return this.on(event, listener);
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.removeListener(event, wrapper);
      listener(...args);
    };
    wrapper._originalListener = listener;
    return this.on(event, wrapper);
  }
  off(event, listener) {
    return this.removeListener(event, listener);
  }
  removeListener(event, listener) {
    const listeners = this._listeners.get(event);
    if (!listeners) {
      return this;
    }
    const index = listeners.findIndex(
      (candidate) => candidate === listener || candidate._originalListener === listener
    );
    if (index >= 0) {
      listeners.splice(index, 1);
    }
    if (listeners.length === 0) {
      this._listeners.delete(event);
    }
    return this;
  }
  removeAllListeners(event) {
    if (event === void 0) {
      this._listeners.clear();
    } else {
      this._listeners.delete(event);
    }
    return this;
  }
  emit(event, ...args) {
    const listeners = this._listeners.get(event);
    if (!listeners?.length) {
      return false;
    }
    listeners.slice().forEach((listener) => listener(...args));
    return true;
  }
  ref() {
    this._persistent = true;
    this._timer?.ref?.();
    return this;
  }
  unref() {
    this._persistent = false;
    this._timer?.unref?.();
    return this;
  }
  close() {
    if (this._closed) {
      return;
    }
    this._closed = true;
    if (this._timer !== void 0) {
      clearTimeout2(this._timer);
      this._timer = void 0;
    }
    if (this._signal) {
      this._signal.removeEventListener("abort", this._handleAbort);
    }
    this._onClose?.();
    this.emit("close");
  }
};
function registerStatWatcher(path, watcher) {
  const watchers = activeStatWatchers.get(path) ?? /* @__PURE__ */ new Set();
  watchers.add(watcher);
  activeStatWatchers.set(path, watchers);
}
function unregisterStatWatcher(path, watcher) {
  const watchers = activeStatWatchers.get(path);
  if (!watchers) {
    return;
  }
  watchers.delete(watcher);
  if (watchers.size === 0) {
    activeStatWatchers.delete(path);
  }
}
function createFsWatcher(path, options) {
  const filename = createWatcherFilename(path, options.encoding);
  const watcher = new PollingFsWatcher(path, {
    interval: DEFAULT_FS_WATCH_INTERVAL_MS,
    maxInterval: MAX_IDLE_FS_WATCH_INTERVAL_MS,
    persistent: options.persistent,
    signal: options.signal,
    onChange(current, previous) {
      watcher.emit("change", watcherEventType(previous, current), filename);
    }
  });
  return watcher;
}
function createFsStatWatcher(path, options, listener) {
  const watcher = new PollingFsWatcher(path, {
    interval: options.interval ?? DEFAULT_FS_WATCH_FILE_INTERVAL_MS,
    persistent: options.persistent,
    onChange(current, previous) {
      watcher.emit("change", current.stats, previous.stats);
    },
    onClose() {
      unregisterStatWatcher(path, watcher);
    }
  });
  watcher.on("change", listener);
  registerStatWatcher(path, watcher);
  return watcher;
}
async function* createPromisesWatchIterator(path, options) {
  const events = [];
  let wake = null;
  let closed = false;
  let thrown = null;
  const watcher = fs.watch(path, options, (eventType, filename) => {
    events.push({ eventType, filename });
    wake?.();
    wake = null;
  });
  watcher.on("close", () => {
    closed = true;
    wake?.();
    wake = null;
  });
  watcher.on("error", (error) => {
    thrown = error;
    wake?.();
    wake = null;
  });
  try {
    while (true) {
      if (events.length > 0) {
        yield events.shift();
        continue;
      }
      if (thrown) {
        throw thrown;
      }
      if (closed) {
        return;
      }
      await new Promise((resolve) => {
        wake = resolve;
      });
    }
  } finally {
    watcher.close();
  }
}
function isReadWriteOptionsObject(value) {
  return value === null || value === void 0 || typeof value === "object" && !Array.isArray(value);
}
function normalizeOptionalPosition(value) {
  if (value === void 0 || value === null || value === -1) {
    return null;
  }
  if (typeof value === "bigint") {
    return Number(value);
  }
  if (typeof value !== "number" || !Number.isInteger(value)) {
    throw createInvalidArgTypeError("position", "an integer", value);
  }
  return value;
}
function normalizeOffsetLength(bufferByteLength, offsetValue, lengthValue) {
  const offset = offsetValue ?? 0;
  if (typeof offset !== "number" || !Number.isInteger(offset)) {
    throw createInvalidArgTypeError("offset", "an integer", offset);
  }
  if (offset < 0 || offset > bufferByteLength) {
    throw createOutOfRangeError("offset", `>= 0 && <= ${bufferByteLength}`, offset);
  }
  const defaultLength = bufferByteLength - offset;
  const length = lengthValue ?? defaultLength;
  if (typeof length !== "number" || !Number.isInteger(length)) {
    throw createInvalidArgTypeError("length", "an integer", length);
  }
  if (length < 0 || length > 2147483647) {
    throw createOutOfRangeError("length", ">= 0 && <= 2147483647", length);
  }
  if (offset + length > bufferByteLength) {
    throw createOutOfRangeError("length", `>= 0 && <= ${bufferByteLength - offset}`, length);
  }
  return { offset, length };
}
function normalizeReadSyncArgs(buffer, offsetOrOptions, length, position) {
  if (!isArrayBufferView(buffer)) {
    throw createInvalidArgTypeError("buffer", "an instance of Buffer, TypedArray, or DataView", buffer);
  }
  if (length === void 0 && position === void 0 && isReadWriteOptionsObject(offsetOrOptions)) {
    const options = offsetOrOptions ?? {};
    const { offset: offset2, length: length2 } = normalizeOffsetLength(
      buffer.byteLength,
      options.offset,
      options.length
    );
    return {
      buffer,
      offset: offset2,
      length: length2,
      position: normalizeOptionalPosition(options.position)
    };
  }
  const { offset, length: normalizedLength } = normalizeOffsetLength(
    buffer.byteLength,
    offsetOrOptions,
    length
  );
  return {
    buffer,
    offset,
    length: normalizedLength,
    position: normalizeOptionalPosition(position)
  };
}
function normalizeWriteSyncArgs(buffer, offsetOrPosition, lengthOrEncoding, position) {
  if (typeof buffer === "string") {
    if (lengthOrEncoding === void 0 && position === void 0 && isReadWriteOptionsObject(offsetOrPosition)) {
      const options = offsetOrPosition ?? {};
      const encoding = typeof options.encoding === "string" ? options.encoding : void 0;
      return {
        buffer,
        offset: 0,
        length: import_buffer.Buffer.byteLength(buffer, encoding),
        position: normalizeOptionalPosition(options.position),
        encoding
      };
    }
    if (offsetOrPosition !== void 0 && offsetOrPosition !== null && typeof offsetOrPosition !== "number") {
      throw createInvalidArgTypeError("position", "an integer", offsetOrPosition);
    }
    return {
      buffer,
      offset: 0,
      length: import_buffer.Buffer.byteLength(buffer, typeof lengthOrEncoding === "string" ? lengthOrEncoding : void 0),
      position: normalizeOptionalPosition(offsetOrPosition),
      encoding: typeof lengthOrEncoding === "string" ? lengthOrEncoding : void 0
    };
  }
  if (!isArrayBufferView(buffer)) {
    throw createInvalidArgTypeError("buffer", "a string, Buffer, TypedArray, or DataView", buffer);
  }
  if (lengthOrEncoding === void 0 && position === void 0 && isReadWriteOptionsObject(offsetOrPosition)) {
    const options = offsetOrPosition ?? {};
    const { offset: offset2, length: length2 } = normalizeOffsetLength(
      buffer.byteLength,
      options.offset,
      options.length
    );
    return {
      buffer,
      offset: offset2,
      length: length2,
      position: normalizeOptionalPosition(options.position)
    };
  }
  const { offset, length } = normalizeOffsetLength(
    buffer.byteLength,
    offsetOrPosition,
    typeof lengthOrEncoding === "number" ? lengthOrEncoding : void 0
  );
  return {
    buffer,
    offset,
    length,
    position: normalizeOptionalPosition(position)
  };
}
function normalizeFdInteger(fd) {
  return normalizeNumberArgument("fd", fd);
}
function normalizeIoVectorBuffers(buffers) {
  if (!Array.isArray(buffers)) {
    throw createInvalidArgTypeError("buffers", "an ArrayBufferView[]", buffers);
  }
  for (const buffer of buffers) {
    if (!isArrayBufferView(buffer)) {
      throw createInvalidArgTypeError("buffers", "an ArrayBufferView[]", buffers);
    }
  }
  return buffers;
}
function validateStreamFsOverride(streamFs, required) {
  if (streamFs === void 0) {
    return void 0;
  }
  if (streamFs === null || typeof streamFs !== "object") {
    throw createInvalidArgTypeError("options.fs", "an object", streamFs);
  }
  const typed = streamFs;
  for (const key of required) {
    if (typeof typed[key] !== "function") {
      throw createInvalidPropertyTypeError(`options.fs.${String(key)}`, typed[key]);
    }
  }
  return typed;
}
function normalizeStreamFd(fd) {
  if (fd === void 0) {
    return void 0;
  }
  if (fd instanceof FileHandle) {
    return fd;
  }
  return normalizeNumberArgument("fd", fd);
}
function normalizeStreamPath(pathValue, fd) {
  if (pathValue === null) {
    if (fd === void 0) {
      throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", pathValue);
    }
    return null;
  }
  if (typeof pathValue === "string" || import_buffer.Buffer.isBuffer(pathValue)) {
    return pathValue;
  }
  if (pathValue instanceof URL) {
    if (pathValue.protocol === "file:") {
      return pathValue.pathname;
    }
    throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", pathValue);
  }
  throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", pathValue);
}
function normalizeStreamStartEnd(options) {
  const start = options?.start;
  const end = options?.end;
  if (start !== void 0 && typeof start !== "number") {
    throw createInvalidArgTypeError("start", "of type number", start);
  }
  if (end !== void 0 && typeof end !== "number") {
    throw createInvalidArgTypeError("end", "of type number", end);
  }
  const normalizedStart = start;
  const normalizedEnd = end;
  if (normalizedStart !== void 0 && (!Number.isFinite(normalizedStart) || normalizedStart < 0)) {
    throw createOutOfRangeError("start", ">= 0", start);
  }
  if (normalizedEnd !== void 0 && (!Number.isFinite(normalizedEnd) || normalizedEnd < 0)) {
    throw createOutOfRangeError("end", ">= 0", end);
  }
  if (normalizedStart !== void 0 && normalizedEnd !== void 0 && normalizedStart > normalizedEnd) {
    throw createOutOfRangeError("start", `<= "end" (here: ${normalizedEnd})`, normalizedStart);
  }
  const highWaterMarkCandidate = options?.highWaterMark ?? options?.bufferSize;
  const highWaterMark = typeof highWaterMarkCandidate === "number" && Number.isFinite(highWaterMarkCandidate) && highWaterMarkCandidate > 0 ? Math.floor(highWaterMarkCandidate) : 65536;
  return {
    start: normalizedStart,
    end: normalizedEnd,
    highWaterMark,
    autoClose: options?.autoClose !== false
  };
}
var ReadStream = class {
  constructor(filePath, _options) {
    this._options = _options;
    const fdOption = normalizeStreamFd(_options?.fd);
    const optionsRecord = _options ?? {};
    const streamState = normalizeStreamStartEnd(optionsRecord);
    this.path = filePath;
    this.start = streamState.start;
    this.end = streamState.end;
    this.autoClose = streamState.autoClose;
    this.readableHighWaterMark = streamState.highWaterMark;
    this.readableEncoding = _options?.encoding ?? null;
    this._position = this.start ?? null;
    this._remaining = this.end !== void 0 ? this.end - (this.start ?? 0) + 1 : null;
    this._signal = validateAbortSignal(_options?.signal);
    if (fdOption instanceof FileHandle) {
      if (_options?.fs !== void 0) {
        const error = new Error("The FileHandle with fs method is not implemented");
        error.code = "ERR_METHOD_NOT_IMPLEMENTED";
        throw error;
      }
      this._fileHandle = fdOption;
      this.fd = fdOption.fd;
      this.pending = false;
      this._handleCloseListener = () => {
        if (!this.closed) {
          this.closed = true;
          this.destroyed = true;
          this.readable = false;
          this.emit("close");
        }
      };
      this._fileHandle.on("close", this._handleCloseListener);
    } else {
      this._streamFs = validateStreamFsOverride(_options?.fs, ["open", "read", "close"]);
      if (typeof fdOption === "number") {
        this.fd = fdOption;
        this.pending = false;
      }
    }
    if (this._signal) {
      if (this._signal.aborted) {
        queueMicrotask(() => {
          void this._abort(this._signal?.reason);
        });
      } else {
        this._signal.addEventListener("abort", () => {
          void this._abort(this._signal?.reason);
        });
      }
    }
    if (this.fd === null) {
      queueMicrotask(() => {
        void this._openIfNeeded();
      });
    }
  }
  _options;
  bytesRead = 0;
  path;
  pending = true;
  readable = true;
  readableAborted = false;
  readableDidRead = false;
  readableEncoding = null;
  readableEnded = false;
  readableFlowing = null;
  readableHighWaterMark = 65536;
  readableLength = 0;
  readableObjectMode = false;
  destroyed = false;
  closed = false;
  errored = null;
  fd = null;
  autoClose = true;
  start;
  end;
  _listeners = /* @__PURE__ */ new Map();
  _started = false;
  _reading = false;
  _readScheduled = false;
  _opening = false;
  _remaining = null;
  _position = null;
  _fileHandle = null;
  _streamFs;
  _signal;
  _handleCloseListener;
  _emitOpen(fd) {
    this.fd = fd;
    this.pending = false;
    this.emit("open", fd);
    if (this._started || this.readableFlowing) {
      this._scheduleRead();
    }
  }
  async _openIfNeeded() {
    if (this.fd !== null || this._opening || this.destroyed || this.closed) {
      return;
    }
    const pathStr = typeof this.path === "string" ? this.path : this.path instanceof import_buffer.Buffer ? this.path.toString() : null;
    if (!pathStr) {
      this._handleStreamError(createFsError("EBADF", "EBADF: bad file descriptor", "read"));
      return;
    }
    this._opening = true;
    const opener = (this._streamFs?.open ?? fs.open).bind(this._streamFs ?? fs);
    opener(pathStr, "r", 438, (error, fd) => {
      this._opening = false;
      if (error || typeof fd !== "number") {
        this._handleStreamError(error ?? createFsError("EBADF", "EBADF: bad file descriptor", "open"));
        return;
      }
      this._emitOpen(fd);
    });
  }
  async _closeUnderlying() {
    if (this._fileHandle) {
      if (!this._fileHandle.closed) {
        await this._fileHandle.close();
      }
      return;
    }
    if (this.fd !== null && this.fd >= 0) {
      const fd = this.fd;
      const closer = (this._streamFs?.close ?? fs.close).bind(this._streamFs ?? fs);
      await new Promise((resolve) => {
        closer(fd, () => resolve());
      });
      this.fd = -1;
    }
  }
  _scheduleRead() {
    if (this._readScheduled || this._reading || this.readableFlowing === false || this.destroyed || this.closed) {
      return;
    }
    this._readScheduled = true;
    queueMicrotask(() => {
      this._readScheduled = false;
      void this._readNextChunk();
    });
  }
  async _readNextChunk() {
    if (this._reading || this.destroyed || this.closed || this.readableFlowing === false) {
      return;
    }
    throwIfAborted(this._signal);
    if (this.fd === null) {
      await this._openIfNeeded();
      return;
    }
    if (this._remaining === 0) {
      await this._finishReadable();
      return;
    }
    const nextLength = this._remaining === null ? this.readableHighWaterMark : Math.min(this.readableHighWaterMark, this._remaining);
    const target = import_buffer.Buffer.alloc(nextLength);
    this._reading = true;
    const onRead = async (error, bytesRead = 0) => {
      this._reading = false;
      if (error) {
        this._handleStreamError(error);
        return;
      }
      if (bytesRead === 0) {
        await this._finishReadable();
        return;
      }
      this.bytesRead += bytesRead;
      this.readableDidRead = true;
      if (typeof this._position === "number") {
        this._position += bytesRead;
      }
      if (this._remaining !== null) {
        this._remaining -= bytesRead;
      }
      const chunk = target.subarray(0, bytesRead);
      this.emit("data", this.readableEncoding ? chunk.toString(this.readableEncoding) : import_buffer.Buffer.from(chunk));
      if (this._remaining === 0) {
        await this._finishReadable();
        return;
      }
      this._scheduleRead();
    };
    if (this._fileHandle) {
      try {
        const result = await this._fileHandle.read(target, 0, nextLength, this._position);
        await onRead(null, result.bytesRead);
      } catch (error) {
        await onRead(error);
      }
      return;
    }
    const reader = (this._streamFs?.read ?? fs.read).bind(this._streamFs ?? fs);
    reader(this.fd, target, 0, nextLength, this._position, (error, bytesRead) => {
      void onRead(error, bytesRead ?? 0);
    });
  }
  async _finishReadable() {
    if (this.readableEnded) {
      return;
    }
    this.readable = false;
    this.readableEnded = true;
    this.emit("end");
    if (this.autoClose) {
      this.destroy();
    }
  }
  _handleStreamError(error) {
    if (this.closed) {
      return;
    }
    this.errored = error;
    this.emit("error", error);
    if (this.autoClose) {
      this.destroy();
    } else {
      this.readable = false;
    }
  }
  async _abort(reason) {
    if (this.closed || this.destroyed) {
      return;
    }
    this.readableAborted = true;
    this.errored = createAbortError(reason);
    this.emit("error", this.errored);
    if (this._fileHandle) {
      this.destroyed = true;
      this.readable = false;
      this.closed = true;
      this.emit("close");
      return;
    }
    if (this.autoClose) {
      this.destroy();
      return;
    }
    this.closed = true;
    this.emit("close");
  }
  async _readAllContent() {
    const chunks = [];
    let totalLength = 0;
    const savedFlowing = this.readableFlowing;
    this.readableFlowing = false;
    while (this._remaining !== 0) {
      if (this.fd === null) {
        await this._openIfNeeded();
      }
      if (this.fd === null) {
        break;
      }
      const nextLength = this._remaining === null ? FILE_HANDLE_READ_CHUNK_BYTES : Math.min(FILE_HANDLE_READ_CHUNK_BYTES, this._remaining);
      const target = import_buffer.Buffer.alloc(nextLength);
      let bytesRead = 0;
      if (this._fileHandle) {
        bytesRead = (await this._fileHandle.read(target, 0, nextLength, this._position)).bytesRead;
      } else {
        bytesRead = fs.readSync(this.fd, target, 0, nextLength, this._position);
      }
      if (bytesRead === 0) {
        break;
      }
      const chunk = target.subarray(0, bytesRead);
      chunks.push(chunk);
      totalLength += bytesRead;
      if (typeof this._position === "number") {
        this._position += bytesRead;
      }
      if (this._remaining !== null) {
        this._remaining -= bytesRead;
      }
    }
    this.readableFlowing = savedFlowing;
    return import_buffer.Buffer.concat(chunks, totalLength);
  }
  on(event, listener) {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    if (event === "data") {
      this._started = true;
      if (this.readableFlowing !== false) {
        this.readableFlowing = true;
        this._scheduleRead();
      }
    }
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    wrapper._originalListener = listener;
    return this.on(event, wrapper);
  }
  off(event, listener) {
    const listeners = this._listeners.get(event);
    if (!listeners) {
      return this;
    }
    const index = listeners.findIndex(
      (fn) => fn === listener || fn._originalListener === listener
    );
    if (index >= 0) {
      listeners.splice(index, 1);
    }
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  removeAllListeners(event) {
    if (event === void 0) {
      this._listeners.clear();
    } else {
      this._listeners.delete(event);
    }
    return this;
  }
  emit(event, ...args) {
    const listeners = this._listeners.get(event);
    if (!listeners?.length) {
      return false;
    }
    listeners.slice().forEach((listener) => listener(...args));
    return true;
  }
  read() {
    return null;
  }
  pipe(destination, _options) {
    this.on("data", (chunk) => {
      destination.write(chunk);
    });
    this.on("end", () => {
      destination.end?.();
    });
    this.resume();
    return destination;
  }
  unpipe(_destination) {
    return this;
  }
  pause() {
    this.readableFlowing = false;
    return this;
  }
  resume() {
    this._started = true;
    this.readableFlowing = true;
    this._scheduleRead();
    return this;
  }
  setEncoding(encoding) {
    this.readableEncoding = encoding;
    return this;
  }
  destroy(error) {
    if (this.destroyed) {
      return this;
    }
    this.destroyed = true;
    this.readable = false;
    if (error) {
      this.errored = error;
      this.emit("error", error);
    }
    queueMicrotask(() => {
      void this._closeUnderlying().then(() => {
        if (!this.closed) {
          this.closed = true;
          this.emit("close");
        }
      });
    });
    return this;
  }
  close(callback) {
    this.destroy();
    if (callback) {
      queueMicrotask(() => callback(null));
    }
  }
  async *[Symbol.asyncIterator]() {
    const content = await this._readAllContent();
    yield this.readableEncoding ? content.toString(this.readableEncoding) : content;
  }
};
var MAX_WRITE_STREAM_BYTES = 16 * 1024 * 1024;
var WriteStream = class {
  constructor(filePath, _options) {
    this._options = _options;
    const fdOption = normalizeStreamFd(_options?.fd);
    const startOption = _options?.start;
    const highWaterMarkCandidate = _options?.highWaterMark ?? _options?.bufferSize;
    const openFlags = _options?.flags ?? "w";
    this.path = filePath;
    this.autoClose = _options?.autoClose !== false;
    this.writableHighWaterMark = typeof highWaterMarkCandidate === "number" && Number.isFinite(highWaterMarkCandidate) && highWaterMarkCandidate > 0 ? Math.floor(highWaterMarkCandidate) : 16384;
    this._position = typeof startOption === "number" ? startOption : null;
    this._streamFs = validateStreamFsOverride(_options?.fs, ["open", "close", "write"]);
    if (_options?.fs !== void 0) {
      validateStreamFsOverride(_options?.fs, ["writev"]);
    }
    if (fdOption instanceof FileHandle) {
      this._fileHandle = fdOption;
      this.fd = fdOption.fd;
      return;
    }
    if (typeof fdOption === "number") {
      this.fd = fdOption;
      return;
    }
    const pathStr = typeof this.path === "string" ? this.path : this.path instanceof import_buffer.Buffer ? this.path.toString() : null;
    if (!pathStr) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
    }
    this.fd = fs.openSync(pathStr, openFlags, _options?.mode);
    queueMicrotask(() => {
      if (this.fd !== null && this.fd >= 0) {
        this.emit("open", this.fd);
      }
    });
  }
  _options;
  bytesWritten = 0;
  path;
  pending = false;
  writable = true;
  writableAborted = false;
  writableEnded = false;
  writableFinished = false;
  writableHighWaterMark = 16384;
  writableLength = 0;
  writableObjectMode = false;
  writableCorked = 0;
  destroyed = false;
  closed = false;
  errored = null;
  writableNeedDrain = false;
  fd = null;
  autoClose = true;
  _chunks = [];
  _listeners = /* @__PURE__ */ new Map();
  _fileHandle = null;
  _streamFs;
  _position = null;
  async _closeUnderlying() {
    if (this._fileHandle) {
      if (!this._fileHandle.closed) {
        await this._fileHandle.close();
      }
      return;
    }
    if (this.fd !== null && this.fd >= 0) {
      const fd = this.fd;
      const closer = (this._streamFs?.close ?? fs.close).bind(this._streamFs ?? fs);
      await new Promise((resolve) => {
        closer(fd, () => resolve());
      });
      this.fd = -1;
    }
  }
  close(callback) {
    queueMicrotask(() => {
      void this._closeUnderlying().then(() => {
        if (!this.closed) {
          this.closed = true;
          this.writable = false;
          this.emit("close");
        }
        callback?.(null);
      });
    });
  }
  write(chunk, encodingOrCallback, callback) {
    if (this.writableEnded || this.destroyed) {
      const error = new Error("write after end");
      const cb2 = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      queueMicrotask(() => cb2?.(error));
      return false;
    }
    let data;
    if (typeof chunk === "string") {
      data = import_buffer.Buffer.from(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : "utf8");
    } else if (isArrayBufferView(chunk)) {
      data = new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
    } else {
      throw createInvalidArgTypeError("chunk", "a string, Buffer, TypedArray, or DataView", chunk);
    }
    if (this.writableLength + data.length > MAX_WRITE_STREAM_BYTES) {
      const error = new Error(`WriteStream buffer exceeded ${MAX_WRITE_STREAM_BYTES} bytes`);
      this.errored = error;
      this.destroyed = true;
      this.writable = false;
      const cb2 = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      queueMicrotask(() => {
        cb2?.(error);
        this.emit("error", error);
      });
      return false;
    }
    this._chunks.push(data);
    this.bytesWritten += data.length;
    this.writableLength += data.length;
    const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    queueMicrotask(() => cb?.(null));
    return true;
  }
  end(chunkOrCb, encodingOrCallback, callback) {
    if (this.writableEnded) {
      return this;
    }
    let cb;
    if (typeof chunkOrCb === "function") {
      cb = chunkOrCb;
    } else if (typeof encodingOrCallback === "function") {
      cb = encodingOrCallback;
      if (chunkOrCb !== void 0 && chunkOrCb !== null) {
        this.write(chunkOrCb);
      }
    } else {
      cb = callback;
      if (chunkOrCb !== void 0 && chunkOrCb !== null) {
        this.write(chunkOrCb, encodingOrCallback);
      }
    }
    this.writableEnded = true;
    this.writable = false;
    this.writableFinished = true;
    this.writableLength = 0;
    queueMicrotask(() => {
      void (async () => {
        try {
          if (this._fileHandle) {
            for (const chunk of this._chunks) {
              const result = await this._fileHandle.write(
                chunk,
                0,
                chunk.byteLength,
                this._position
              );
              if (typeof this._position === "number") {
                this._position += result?.bytesWritten ?? chunk.byteLength;
              }
            }
            if (this.autoClose && !this._fileHandle.closed) {
              await this._fileHandle.close();
            }
          } else {
            const pathStr = typeof this.path === "string" ? this.path : this.path instanceof import_buffer.Buffer ? this.path.toString() : null;
            if (this.fd !== null && this.fd >= 0) {
              const bytesWritten = fs.writevSync(this.fd, this._chunks, this._position);
              if (typeof this._position === "number") {
                this._position += bytesWritten;
              }
              if (this.autoClose) {
                await this._closeUnderlying();
              }
            } else if (pathStr) {
              const chunks = this._chunks.map((chunk) => import_buffer.Buffer.from(chunk));
              if (typeof this._position === "number") {
                const existing = fs.readFileSync(pathStr);
                const finalSize = Math.max(
                  existing.length,
                  this._position + chunks.reduce((sum, chunk) => sum + chunk.length, 0)
                );
                const output = import_buffer.Buffer.alloc(finalSize);
                existing.copy(output);
                let cursor = this._position;
                for (const chunk of chunks) {
                  chunk.copy(output, cursor);
                  cursor += chunk.length;
                }
                fs.writeFileSync(pathStr, output);
              } else {
                fs.writeFileSync(
                  pathStr,
                  import_buffer.Buffer.concat(chunks)
                );
              }
              if (this.autoClose && this.fd !== null && this.fd >= 0) {
                await this._closeUnderlying();
              }
            } else {
              throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
            }
          }
          this.emit("finish");
          if (this.autoClose && !this.closed) {
            this.closed = true;
            this.emit("close");
          }
          cb?.();
        } catch (error) {
          this.errored = error;
          this.emit("error", error);
        }
      })();
    });
    return this;
  }
  setDefaultEncoding(_encoding) {
    return this;
  }
  cork() {
    this.writableCorked++;
  }
  uncork() {
    if (this.writableCorked > 0) {
      this.writableCorked--;
    }
  }
  destroy(error) {
    if (this.destroyed) {
      return this;
    }
    this.destroyed = true;
    this.writable = false;
    if (error) {
      this.errored = error;
      this.emit("error", error);
    }
    queueMicrotask(() => {
      void this._closeUnderlying().then(() => {
        if (!this.closed) {
          this.closed = true;
          this.emit("close");
        }
      });
    });
    return this;
  }
  addListener(event, listener) {
    return this.on(event, listener);
  }
  on(event, listener) {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.removeListener(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }
  removeListener(event, listener) {
    const listeners = this._listeners.get(event);
    if (!listeners) {
      return this;
    }
    const index = listeners.indexOf(listener);
    if (index >= 0) {
      listeners.splice(index, 1);
    }
    return this;
  }
  off(event, listener) {
    return this.removeListener(event, listener);
  }
  removeAllListeners(event) {
    if (event === void 0) {
      this._listeners.clear();
    } else {
      this._listeners.delete(event);
    }
    return this;
  }
  emit(event, ...args) {
    const listeners = this._listeners.get(event);
    if (!listeners?.length) {
      return false;
    }
    listeners.slice().forEach((listener) => listener(...args));
    return true;
  }
  pipe(destination, _options) {
    return destination;
  }
  unpipe(_destination) {
    return this;
  }
  [Symbol.asyncDispose]() {
    return Promise.resolve();
  }
};
var ReadStreamClass = ReadStream;
var WriteStreamClass = WriteStream;
var ReadStreamFactory = function ReadStream2(path, options) {
  validateEncodingOption(options);
  return new ReadStreamClass(path, options);
};
ReadStreamFactory.prototype = ReadStream.prototype;
var WriteStreamFactory = function WriteStream2(path, options) {
  validateEncodingOption(options);
  validateWriteStreamStartOption(options ?? {});
  return new WriteStreamClass(path, options);
};
WriteStreamFactory.prototype = WriteStream.prototype;
function parseFlags(flags) {
  if (typeof flags === "number") return flags;
  const flagMap = {
    r: O_RDONLY,
    "r+": O_RDWR,
    rs: O_RDONLY,
    "rs+": O_RDWR,
    w: O_WRONLY | O_CREAT | O_TRUNC,
    "w+": O_RDWR | O_CREAT | O_TRUNC,
    a: O_WRONLY | O_APPEND | O_CREAT,
    "a+": O_RDWR | O_APPEND | O_CREAT,
    wx: O_WRONLY | O_CREAT | O_TRUNC | O_EXCL,
    xw: O_WRONLY | O_CREAT | O_TRUNC | O_EXCL,
    "wx+": O_RDWR | O_CREAT | O_TRUNC | O_EXCL,
    "xw+": O_RDWR | O_CREAT | O_TRUNC | O_EXCL,
    ax: O_WRONLY | O_APPEND | O_CREAT | O_EXCL,
    xa: O_WRONLY | O_APPEND | O_CREAT | O_EXCL,
    "ax+": O_RDWR | O_APPEND | O_CREAT | O_EXCL,
    "xa+": O_RDWR | O_APPEND | O_CREAT | O_EXCL
  };
  if (flags in flagMap) return flagMap[flags];
  throw new Error("Unknown file flag: " + flags);
}
// Full Linux errno table so guest `err.errno` matches real Node for every
// code (Node reports the negated Linux errno), not just the handful the old
// inline ternary covered. The structured `code` from the kernel is the source
// of truth; this maps it to the canonical number.
const POSIX_ERRNO = {
  EPERM: 1, ENOENT: 2, ESRCH: 3, EINTR: 4, EIO: 5, ENXIO: 6, E2BIG: 7,
  ENOEXEC: 8, EBADF: 9, ECHILD: 10, EAGAIN: 11, EWOULDBLOCK: 11, ENOMEM: 12,
  EACCES: 13, EFAULT: 14, ENOTBLK: 15, EBUSY: 16, EEXIST: 17, EXDEV: 18,
  ENODEV: 19, ENOTDIR: 20, EISDIR: 21, EINVAL: 22, ENFILE: 23, EMFILE: 24,
  ENOTTY: 25, ETXTBSY: 26, EFBIG: 27, ENOSPC: 28, ESPIPE: 29, EROFS: 30,
  EMLINK: 31, EPIPE: 32, EDOM: 33, ERANGE: 34, ENAMETOOLONG: 36, ENOSYS: 38,
  ENOTEMPTY: 39, ELOOP: 40, EOVERFLOW: 75, ENOTSOCK: 88, EDESTADDRREQ: 89,
  EMSGSIZE: 90, EPROTOTYPE: 91, ENOPROTOOPT: 92, EPROTONOSUPPORT: 93,
  ENOTSUP: 95, EOPNOTSUPP: 95, EAFNOSUPPORT: 97, EADDRINUSE: 98,
  EADDRNOTAVAIL: 99, ENETDOWN: 100, ENETUNREACH: 101, ECONNABORTED: 103,
  ECONNRESET: 104, ENOBUFS: 105, EISCONN: 106, ENOTCONN: 107, ETIMEDOUT: 110,
  ECONNREFUSED: 111, EHOSTUNREACH: 113, EALREADY: 114, EINPROGRESS: 115,
};
function errnoForCode(code) {
  return Object.prototype.hasOwnProperty.call(POSIX_ERRNO, code)
    ? -POSIX_ERRNO[code]
    : -1;
}
function createFsError(code, message, syscall, path) {
  const err = new Error(message);
  err.code = code;
  err.errno = errnoForCode(code);
  err.syscall = syscall;
  if (path) err.path = path;
  return err;
}
function bridgeErrorText(err) {
  return String(err?.message ?? err ?? "");
}
function bridgeErrorCode(err) {
  const msg = bridgeErrorText(err);
  if (msg.includes("ENOENT") || msg.includes("entry not found") || msg.includes("no such file or directory") || msg.includes("not found")) {
    return "ENOENT";
  }
  if (msg.includes("EROFS") || msg.includes("read-only file system")) {
    return "EROFS";
  }
  if (msg.includes("ERR_ACCESS_DENIED")) {
    return "ERR_ACCESS_DENIED";
  }
  if (msg.includes("EACCES") || msg.includes("permission denied")) {
    return "EACCES";
  }
  if (msg.includes("EEXIST") || msg.includes("file already exists")) {
    return "EEXIST";
  }
  if (msg.includes("EINVAL") || msg.includes("invalid argument")) {
    return "EINVAL";
  }
  if (msg.includes("ENXIO") || msg.includes("no such device or address")) {
    return "ENXIO";
  }
  if (msg.includes("EXDEV") || msg.includes("cross-device link")) {
    return "EXDEV";
  }
  if (typeof err?.code === "string" && err.code.length > 0) {
    return err.code;
  }
  return null;
}
function bridgeCall(fn, syscall, path) {
  try {
    return fn();
  } catch (err) {
    const code = bridgeErrorCode(err);
    if (code === "ENOENT") {
      throw createFsError("ENOENT", `ENOENT: no such file or directory, ${syscall} '${path}'`, syscall, path);
    }
    if (code === "EACCES") {
      throw createFsError("EACCES", `EACCES: permission denied, ${syscall} '${path}'`, syscall, path);
    }
    if (code === "EEXIST") {
      throw createFsError("EEXIST", `EEXIST: file already exists, ${syscall} '${path}'`, syscall, path);
    }
    if (code === "EINVAL") {
      throw createFsError("EINVAL", `EINVAL: invalid argument, ${syscall} '${path}'`, syscall, path);
    }
    if (code === "EXDEV") {
      throw createFsError("EXDEV", `EXDEV: cross-device link not permitted, ${syscall} '${path}'`, syscall, path);
    }
    throw err;
  }
}
function _globToRegex(pattern) {
  let regexStr = "";
  let i = 0;
  while (i < pattern.length) {
    const ch = pattern[i];
    if (ch === "*" && pattern[i + 1] === "*") {
      if (pattern[i + 2] === "/") {
        regexStr += "(?:.+/)?";
        i += 3;
      } else {
        regexStr += ".*";
        i += 2;
      }
    } else if (ch === "*") {
      regexStr += "[^/]*";
      i++;
    } else if (ch === "?") {
      regexStr += "[^/]";
      i++;
    } else if (ch === "{") {
      const close = pattern.indexOf("}", i);
      if (close !== -1) {
        const alternatives = pattern.slice(i + 1, close).split(",");
        regexStr += "(?:" + alternatives.map((a) => a.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\\\*/g, "[^/]*")).join("|") + ")";
        i = close + 1;
      } else {
        regexStr += "\\{";
        i++;
      }
    } else if (ch === "[") {
      const close = pattern.indexOf("]", i);
      if (close !== -1) {
        regexStr += pattern.slice(i, close + 1);
        i = close + 1;
      } else {
        regexStr += "\\[";
        i++;
      }
    } else if (".+^${}()|[]\\".includes(ch)) {
      regexStr += "\\" + ch;
      i++;
    } else {
      regexStr += ch;
      i++;
    }
  }
  return new RegExp("^" + regexStr + "$");
}
function _globGetBase(pattern) {
  const absolute = pattern.startsWith("/");
  const parts = pattern.split("/");
  const baseParts = [];
  for (const part of parts) {
    if (/[*?{}\[\]]/.test(part)) break;
    baseParts.push(part);
  }
  return baseParts.join("/") || (absolute ? "/" : ".");
}
var MAX_GLOB_DEPTH = 100;
function _globJoin(parent, child) {
  if (!child || child === ".") return parent;
  if (!parent || parent === ".") return child;
  if (parent === "/") return `/${child}`;
  return `${parent.replace(/\/$/, "")}/${child}`;
}
function _globExcludeDecision(candidate, entry, options) {
  if (typeof options.exclude === "function") {
    const excluded = options.exclude(options.withFileTypes ? entry : candidate) === true;
    return { excluded, prune: excluded };
  }
  let excluded = false;
  let prune = false;
  for (const regex of options.excludeRegexes) {
    excluded ||= regex.test(candidate);
    prune ||= excluded || regex.test(`${candidate}/`);
  }
  return { excluded, prune };
}
function _globCollect(pattern, options, results) {
  const regex = _globToRegex(pattern);
  const patternIsAbsolute = pattern.startsWith("/");
  const patternBase = _globGetBase(pattern);
  const scanBase = patternIsAbsolute ? patternBase : _globJoin(options.cwd, patternBase);
  const relativeBase = patternBase === "." ? "" : patternBase;
  const addResult = (key, value) => {
    if (!results.has(key)) results.set(key, value);
  };
  if (!/[*?{}\[\]]/.test(pattern)) {
    try {
      const stat = _globStat(scanBase);
      const name = patternBase.split("/").filter(Boolean).pop() ?? patternBase;
      const lastSlash = scanBase.lastIndexOf("/");
      const parent = lastSlash < 0 ? "." : lastSlash === 0 ? "/" : scanBase.slice(0, lastSlash);
      const entry = new Dirent(name, stat.isDirectory(), parent);
      if (!_globExcludeDecision(patternBase, entry, options).excluded) {
        addResult(patternBase, options.withFileTypes ? entry : patternBase);
      }
    } catch {
    }
    return;
  }
  const walk = (dir, relativeDir, depth) => {
    if (depth > MAX_GLOB_DEPTH) return;
    let entries;
    try {
      entries = _globReadDir(dir);
    } catch {
      return;
    }
    for (const entry of entries) {
      const name = typeof entry === "string" ? entry : entry.name;
      const fullPath = _globJoin(dir, name);
      const relativePath = relativeDir ? `${relativeDir}/${name}` : name;
      const candidate = patternIsAbsolute ? fullPath : relativePath;
      const excludeDecision = _globExcludeDecision(candidate, entry, options);
      if (!excludeDecision.excluded && regex.test(candidate)) {
        addResult(candidate, options.withFileTypes ? entry : candidate);
      }
      let isDirectory = typeof entry !== "string" && entry?.isDirectory?.() === true;
      if (typeof entry === "string") {
        try {
          isDirectory = _globStat(fullPath).isDirectory();
        } catch {
        }
      }
      if (isDirectory && !excludeDecision.prune) {
        walk(fullPath, relativePath, depth + 1);
      }
    }
  };
  try {
    walk(scanBase, relativeBase, 0);
  } catch {
  }
}
var _globReadDir;
var _globStat;
function toPathString(path) {
  return normalizePathLike(path);
}
function getBridgeSyncFn(name) {
  return typeof globalThis !== "undefined" ? globalThis[name] : void 0;
}
function createBridgeSyncFacade(name) {
  return {
    applySync(_thisArg, args) {
      const fn = getBridgeSyncFn(name);
      if (typeof fn === "function") {
        return fn(...(args || []));
      }
      if (fn && typeof fn.applySync === "function") {
        return fn.applySync(_thisArg, args);
      }
      return void 0;
    },
    applySyncPromise(_thisArg, args) {
      const fn = getBridgeSyncFn(name);
      if (typeof fn === "function") {
        return fn(...(args || []));
      }
      if (fn && typeof fn.applySync === "function") {
        return fn.applySync(_thisArg, args);
      }
      if (fn && typeof fn.applySyncPromise === "function") {
        return fn.applySyncPromise(_thisArg, args);
      }
      return void 0;
    }
  };
}
function hasBridgeSyncFn(name) {
  const fn = getBridgeSyncFn(name);
  return typeof fn === "function" || !!(fn && (typeof fn.applySync === "function" || typeof fn.applySyncPromise === "function"));
}
function hasBridgeAsyncFn(name) {
  const fn = getBridgeSyncFn(name);
  return typeof fn === "function" || !!(fn && typeof fn.apply === "function");
}
function createBridgeAsyncFacade(name) {
  return {
    apply(_thisArg, args) {
      const fn = getBridgeSyncFn(name);
      if (typeof fn === "function") {
        return fn(...(args || []));
      }
      if (fn && typeof fn.apply === "function") {
        return fn.apply(_thisArg, args);
      }
      return Promise.resolve(void 0);
    }
  };
}
var _fs = {
  readFile: createBridgeSyncFacade("_fsReadFile"),
  writeFile: createBridgeSyncFacade("_fsWriteFile"),
  readFileBinary: createBridgeSyncFacade("_fsReadFileBinary"),
  writeFileBinary: createBridgeSyncFacade("_fsWriteFileBinary"),
  writeFileBinaryRaw: createBridgeSyncFacade("_fsWriteFileBinaryRaw"),
  readDir: createBridgeSyncFacade("_fsReadDir"),
  mkdir: createBridgeSyncFacade("_fsMkdir"),
  rmdir: createBridgeSyncFacade("_fsRmdir"),
  exists: createBridgeSyncFacade("_fsExists"),
  stat: createBridgeSyncFacade("_fsStat"),
  unlink: createBridgeSyncFacade("_fsUnlink"),
  rename: createBridgeSyncFacade("_fsRename"),
  chmod: createBridgeSyncFacade("_fsChmod"),
  chown: createBridgeSyncFacade("_fsChown"),
  link: createBridgeSyncFacade("_fsLink"),
  symlink: createBridgeSyncFacade("_fsSymlink"),
  readlink: createBridgeSyncFacade("_fsReadlink"),
  lstat: createBridgeSyncFacade("_fsLstat"),
  truncate: createBridgeSyncFacade("_fsTruncate"),
  utimes: createBridgeSyncFacade("_fsUtimes"),
  lutimes: createBridgeSyncFacade("_fsLutimes")
};
var _fsAsync = {
  readFile: createBridgeAsyncFacade("_fsReadFileAsync"),
  writeFile: createBridgeAsyncFacade("_fsWriteFileAsync"),
  readFileBinary: createBridgeAsyncFacade("_fsReadFileBinaryAsync"),
  writeFileBinary: createBridgeAsyncFacade("_fsWriteFileBinaryAsync"),
  readDir: createBridgeAsyncFacade("_fsReadDirAsync"),
  mkdir: createBridgeAsyncFacade("_fsMkdirAsync"),
  rmdir: createBridgeAsyncFacade("_fsRmdirAsync"),
  stat: createBridgeAsyncFacade("_fsStatAsync"),
  unlink: createBridgeAsyncFacade("_fsUnlinkAsync"),
  rename: createBridgeAsyncFacade("_fsRenameAsync"),
  chmod: createBridgeAsyncFacade("_fsChmodAsync"),
  chown: createBridgeAsyncFacade("_fsChownAsync"),
  link: createBridgeAsyncFacade("_fsLinkAsync"),
  symlink: createBridgeAsyncFacade("_fsSymlinkAsync"),
  readlink: createBridgeAsyncFacade("_fsReadlinkAsync"),
  lstat: createBridgeAsyncFacade("_fsLstatAsync"),
  truncate: createBridgeAsyncFacade("_fsTruncateAsync"),
  utimes: createBridgeAsyncFacade("_fsUtimesAsync"),
  lutimes: createBridgeAsyncFacade("_fsLutimesAsync"),
  access: createBridgeAsyncFacade("_fsAccessAsync")
};
var _fdOpen = createBridgeSyncFacade("fs.openSync");
var _fdClose = createBridgeSyncFacade("fs.closeSync");
var _fdRead = createBridgeSyncFacade("fs.readSync");
var _fsReadRaw = createBridgeSyncFacade("_fsReadRaw");
var _fsReadFileRangeRaw = createBridgeSyncFacade("_fsReadFileRangeRaw");
var _fdWrite = createBridgeSyncFacade("fs.writeSync");
var _fsWriteRaw = createBridgeSyncFacade("_fsWriteRaw");
var _fsWritevRaw = createBridgeSyncFacade("_fsWritevRaw");
var _fdFstat = createBridgeSyncFacade("fs.fstatSync");
var _fdFtruncate = createBridgeSyncFacade("fs.ftruncateSync");
var _fdFsync = createBridgeSyncFacade("fs.fsyncSync");
var _fdFutimes = createBridgeSyncFacade("fs.futimesSync");
var _fdGetPath = createBridgeSyncFacade("fs._getPathSync");
var _processUmask = createBridgeSyncFacade("process.umask");
var _processMemoryUsage = createBridgeSyncFacade("process.memoryUsage");
var _processCpuUsage = createBridgeSyncFacade("process.cpuUsage");
var _processResourceUsage = createBridgeSyncFacade("process.resourceUsage");
var _processVersions = createBridgeSyncFacade("process.versions");
var _kernelPollRaw = createBridgeSyncFacade("_kernelPollRaw");
var _kernelPoll = createBridgeAsyncFacade("_kernelPoll");
var _kernelIsattyRaw = createBridgeSyncFacade("_kernelIsattyRaw");
var _kernelTtySizeRaw = createBridgeSyncFacade("_kernelTtySizeRaw");
function decodeBridgeJson(value) {
  return typeof value === "string" ? JSON.parse(value) : value;
}
function encodeBridgeBytes(value) {
  return {
    __agentOSType: "bytes",
    base64: import_buffer.Buffer.from(value).toString("base64")
  };
}
function throwNormalizedFsBridgeError(err, syscall, path) {
  const code = bridgeErrorCode(err);
  if (code === "ENOENT") {
    throw createFsError("ENOENT", `ENOENT: no such file or directory, ${syscall} '${path}'`, syscall, path);
  }
  if (code === "EROFS") {
    throw createFsError("EROFS", `EROFS: read-only file system, ${syscall} '${path}'`, syscall, path);
  }
  if (code === "ERR_ACCESS_DENIED") {
    const error = createFsError("ERR_ACCESS_DENIED", `ERR_ACCESS_DENIED: permission denied, ${syscall} '${path}'`, syscall, path);
    error.code = "ERR_ACCESS_DENIED";
    throw error;
  }
  if (code === "EACCES") {
    throw createFsError("EACCES", `EACCES: permission denied, ${syscall} '${path}'`, syscall, path);
  }
  throw err;
}
function joinDirEntryPath(dirPath, entryName) {
  if (dirPath === "/") {
    return `/${entryName}`;
  }
  return dirPath.endsWith("/") ? `${dirPath}${entryName}` : `${dirPath}/${entryName}`;
}
function normalizeReaddirEntries(entries, dirPath, withFileTypes) {
  if (!Array.isArray(entries)) {
    return [];
  }
  if (!withFileTypes) {
    return entries.map((entry) => typeof entry === "string" ? entry : entry?.name);
  }
  return entries.map((entry) => {
    if (typeof entry === "string") {
      const stat = fs.statSync(joinDirEntryPath(dirPath, entry));
      return new Dirent(entry, stat.isDirectory(), dirPath);
    }
    return new Dirent(entry.name, entry.isDirectory, dirPath);
  });
}
function decodeRawReaddirEntries(entries, dirPath, withFileTypes) {
  if (!(entries instanceof Uint8Array)) {
    return normalizeReaddirEntries(entries, dirPath, withFileTypes);
  }
  const decoded = [];
  let offset = 0;
  while (offset < entries.byteLength) {
    if (offset + 5 > entries.byteLength) {
      throw new Error("Invalid raw readdir payload");
    }
    const kind = entries[offset++];
    const nameLength = entries[offset] | entries[offset + 1] << 8 | entries[offset + 2] << 16 | entries[offset + 3] << 24;
    offset += 4;
    if (nameLength < 0 || offset + nameLength > entries.byteLength) {
      throw new Error("Invalid raw readdir entry");
    }
    const name = import_buffer.Buffer.from(
      entries.buffer,
      entries.byteOffset + offset,
      nameLength
    ).toString("utf8");
    offset += nameLength;
    decoded.push(withFileTypes ? new Dirent(name, kind === 1, dirPath) : name);
  }
  return decoded;
}
async function fsReadFileAsync(path, options) {
  validateEncodingOption(options);
  if (typeof path === "number") {
    return new FileHandle(normalizeFdInteger(path)).readFile(options);
  }

  const rawPath = normalizePathLike(path);
  const encoding = typeof options === "string" ? options : options?.encoding;
  try {
    if (encoding) {
      return await _fsAsync.readFile.apply(void 0, [rawPath, encoding]);
    }
    const base64Content = await _fsAsync.readFileBinary.apply(void 0, [rawPath]);
    return import_buffer.Buffer.from(base64Content, "base64");
  } catch (err) {
    if (bridgeErrorCode(err) === "ENOENT") {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, open '${rawPath}'`,
        "open",
        rawPath
      );
    }
    if (bridgeErrorCode(err) === "EACCES") {
      throw createFsError(
        "EACCES",
        `EACCES: permission denied, open '${rawPath}'`,
        "open",
        rawPath
      );
    }
    throw err;
  }
}
async function fsWriteFileAsync(file, data, options) {
  validateEncodingOption(options);
  if (typeof file === "number") {
    return new FileHandle(normalizeFdInteger(file)).writeFile(data, options);
  }
  const rawPath = normalizePathLike(file);
  const pathStr = resolveOperationPath(file);
  try {
    if (typeof data === "string") {
      return await _fsAsync.writeFile.apply(void 0, [pathStr, data]);
    }
    if (ArrayBuffer.isView(data)) {
      const uint8 = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      if (hasBridgeSyncFn("_fsWriteFileBinaryRaw")) {
        return _fs.writeFileBinaryRaw.applySyncPromise(void 0, [pathStr, uint8]);
      }
      return await _fsAsync.writeFileBinary.apply(void 0, [pathStr, encodeBridgeBytes(uint8)]);
    }
    return await _fsAsync.writeFile.apply(void 0, [pathStr, String(data)]);
  } catch (err) {
    throwNormalizedFsBridgeError(err, "write", rawPath);
  }
}
async function fsReaddirAsync(path, options) {
  validateEncodingOption(options);
  const rawPath = normalizePathLike(path);
  try {
    const entriesJson = await _fsAsync.readDir.apply(void 0, [rawPath]);
    return normalizeReaddirEntries(decodeBridgeJson(entriesJson), rawPath, options?.withFileTypes);
  } catch (err) {
    if (bridgeErrorCode(err) === "ENOENT") {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, scandir '${rawPath}'`,
        "scandir",
        rawPath
      );
    }
    throw err;
  }
}
async function fsMkdirAsync(path, options) {
  const rawPath = normalizePathLike(path);
  const recursive = typeof options === "object" ? options?.recursive ?? false : false;
  await _fsAsync.mkdir.apply(void 0, [rawPath, recursive]);
  return recursive ? rawPath : void 0;
}
async function fsRmdirAsync(path) {
  const pathStr = normalizePathLike(path);
  await _fsAsync.rmdir.apply(void 0, [pathStr]);
}
async function fsStatAsync(path) {
  const rawPath = normalizePathLike(path);
  try {
    const statJson = await _fsAsync.stat.apply(void 0, [rawPath]);
    return new Stats(decodeBridgeJson(statJson));
  } catch (err) {
    if (bridgeErrorCode(err) === "ENOENT") {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, stat '${rawPath}'`,
        "stat",
        rawPath
      );
    }
    throw err;
  }
}
async function fsLstatAsync(path) {
  const pathStr = normalizePathLike(path);
  const statJson = await _fsAsync.lstat.apply(void 0, [pathStr]);
  return new Stats(decodeBridgeJson(statJson));
}
async function fsUnlinkAsync(path) {
  const pathStr = normalizePathLike(path);
  await _fsAsync.unlink.apply(void 0, [pathStr]);
}
async function fsRenameAsync(oldPath, newPath) {
  const oldPathStr = normalizePathLike(oldPath, "oldPath");
  const newPathStr = normalizePathLike(newPath, "newPath");
  await _fsAsync.rename.apply(void 0, [oldPathStr, newPathStr]);
}
async function fsAccessAsync(path) {
  const pathStr = normalizePathLike(path);
  try {
    await _fsAsync.access.apply(void 0, [pathStr]);
  } catch (err) {
    if (bridgeErrorCode(err) === "ENOENT") {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, access '${pathStr}'`,
        "access",
        pathStr
      );
    }
    throw err;
  }
}
async function fsChmodAsync(path, mode) {
  const pathStr = normalizePathLike(path);
  const modeNum = normalizeModeArgument(mode, "mode");
  await _fsAsync.chmod.apply(void 0, [pathStr, modeNum]);
}
async function fsChownAsync(path, uid, gid) {
  const pathStr = normalizePathLike(path);
  const normalizedUid = normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
  const normalizedGid = normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
  await _fsAsync.chown.apply(void 0, [pathStr, normalizedUid, normalizedGid]);
}
async function fsLinkAsync(existingPath, newPath) {
  const existingStr = normalizePathLike(existingPath, "existingPath");
  const newStr = normalizePathLike(newPath, "newPath");
  await _fsAsync.link.apply(void 0, [existingStr, newStr]);
}
async function fsSymlinkAsync(target, path) {
  const targetStr = normalizePathLike(target, "target");
  const pathStr = normalizePathLike(path);
  await _fsAsync.symlink.apply(void 0, [targetStr, pathStr]);
}
async function fsReadlinkAsync(path) {
  const pathStr = normalizePathLike(path);
  return await _fsAsync.readlink.apply(void 0, [pathStr]);
}
async function fsTruncateAsync(path, len) {
  const pathStr = normalizePathLike(path);
  await _fsAsync.truncate.apply(void 0, [pathStr, len ?? 0]);
}
function normalizeFsTimeSpec(value, label) {
  if (value && typeof value === "object" && !(value instanceof Date)) {
    const kind = typeof value.kind === "string" ? value.kind : null;
    if (kind === "now" || kind === "UTIME_NOW") {
      return { kind: "now" };
    }
    if (kind === "omit" || kind === "UTIME_OMIT") {
      return { kind: "omit" };
    }
    if ("nsec" in value) {
      if (value.nsec === fs.constants.UTIME_NOW || value.nsec === "UTIME_NOW") {
        return { kind: "now" };
      }
      if (value.nsec === fs.constants.UTIME_OMIT || value.nsec === "UTIME_OMIT") {
        return { kind: "omit" };
      }
    }
    const sec = Number(value.sec);
    const nsec = Number(value.nsec ?? 0);
    if (!Number.isInteger(sec)) {
      throw createInvalidArgTypeError(label, "an integer sec field", value);
    }
    if (!Number.isInteger(nsec) || nsec < 0 || nsec >= 1e9) {
      throw createRangeError(`${label}.nsec must be an integer between 0 and 999999999`);
    }
    return { sec, nsec };
  }
  const seconds = typeof value === "number" ? value : new Date(value).getTime() / 1e3;
  if (!Number.isFinite(seconds)) {
    throw createRangeError(`${label} must be a finite timestamp`);
  }
  const floor = Math.floor(seconds);
  let sec = floor;
  let nsec = Math.round((seconds - floor) * 1e9);
  if (nsec >= 1e9) {
    sec += 1;
    nsec -= 1e9;
  }
  return { sec, nsec };
}
async function fsUtimesAsync(path, atime, mtime) {
  const pathStr = normalizePathLike(path);
  await _fsAsync.utimes.apply(void 0, [
    pathStr,
    normalizeFsTimeSpec(atime, "atime"),
    normalizeFsTimeSpec(mtime, "mtime")
  ]);
}
async function fsLutimesAsync(path, atime, mtime) {
  const pathStr = normalizePathLike(path);
  await _fsAsync.lutimes.apply(void 0, [
    pathStr,
    normalizeFsTimeSpec(atime, "atime"),
    normalizeFsTimeSpec(mtime, "mtime")
  ]);
}
function encodeWritevRawPayload(buffers) {
  let totalBytes = 4;
  for (const buffer of buffers) {
    if (buffer.byteLength > 4294967295) {
      throw createOutOfRangeError("buffer.byteLength", "<= 4294967295", buffer.byteLength);
    }
    totalBytes += 4 + buffer.byteLength;
  }
  const payload = new Uint8Array(totalBytes);
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  view.setUint32(0, buffers.length, true);
  let offset = 4;
  for (const buffer of buffers) {
    const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
    view.setUint32(offset, bytes.byteLength, true);
    offset += 4;
    payload.set(bytes, offset);
    offset += bytes.byteLength;
  }
  return payload;
}
var fs = {
  // Constants
  constants: {
    // File Access Constants
    F_OK: 0,
    R_OK: 4,
    W_OK: 2,
    X_OK: 1,
    // File Copy Constants
    COPYFILE_EXCL: 1,
    COPYFILE_FICLONE: 2,
    COPYFILE_FICLONE_FORCE: 4,
    // File Open Constants
    O_RDONLY,
    O_WRONLY,
    O_RDWR,
    O_CREAT,
    O_EXCL,
    O_NOCTTY: 256,
    O_TRUNC,
    O_APPEND,
    O_DIRECTORY: 65536,
    O_NOATIME: 262144,
    O_NOFOLLOW: 131072,
    O_SYNC: 1052672,
    O_DSYNC: 4096,
    O_SYMLINK: 2097152,
    O_DIRECT: 16384,
    O_NONBLOCK: 2048,
    UTIME_NOW: 1073741823,
    UTIME_OMIT: 1073741822,
    // File Type Constants
    S_IFMT: 61440,
    S_IFREG: 32768,
    S_IFDIR: 16384,
    S_IFCHR: 8192,
    S_IFBLK: 24576,
    S_IFIFO: 4096,
    S_IFLNK: 40960,
    S_IFSOCK: 49152,
    // File Mode Constants
    S_IRWXU: 448,
    S_IRUSR: 256,
    S_IWUSR: 128,
    S_IXUSR: 64,
    S_IRWXG: 56,
    S_IRGRP: 32,
    S_IWGRP: 16,
    S_IXGRP: 8,
    S_IRWXO: 7,
    S_IROTH: 4,
    S_IWOTH: 2,
    S_IXOTH: 1,
    UV_FS_O_FILEMAP: 536870912
  },
  Stats,
  Dirent,
  Dir,
  // Sync methods
  readFileSync(path, options) {
    validateEncodingOption(options);
    const encoding = typeof options === "string" ? options : options?.encoding;
    const suppliedFd = typeof path === "number";
    const rawPath = suppliedFd ? null : normalizePathLike(path);
    const operationPath = suppliedFd ? null : resolveOperationPath(path);
    const fd = suppliedFd ? normalizeFdInteger(path) : null;
    try {
      const chunks = [];
      let totalLength = 0;
      let position = 0;
      while (true) {
        let chunk;
        let bytesRead;
        if (suppliedFd) {
          chunk = import_buffer.Buffer.allocUnsafe(READ_FILE_SYNC_CHUNK_BYTES);
          bytesRead = fs.readSync(fd, chunk, 0, chunk.byteLength, null);
        } else {
          const rawBytes = _fsReadFileRangeRaw.applySyncPromise(void 0, [
            operationPath,
            position,
            READ_FILE_SYNC_CHUNK_BYTES
          ]);
          chunk = rawBytes instanceof Uint8Array ? rawBytes : import_buffer.Buffer.from(rawBytes);
          bytesRead = chunk.byteLength;
        }
        if (bytesRead === 0) {
          break;
        }
        chunks.push(chunk.subarray(0, bytesRead));
        totalLength += bytesRead;
        position += bytesRead;
        if (totalLength > FILE_HANDLE_MAX_READ_BYTES) {
          const error = new RangeError("File size is greater than 2 GiB");
          error.code = "ERR_FS_FILE_TOO_LARGE";
          throw error;
        }
      }
      const content = import_buffer.Buffer.concat(chunks, totalLength);
      return encoding ? content.toString(encoding) : content;
    } catch (err) {
      if (bridgeErrorCode(err) === "ENOENT") {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, open '${rawPath}'`,
          "open",
          rawPath
        );
      }
      if (bridgeErrorCode(err) === "EACCES") {
        throw createFsError(
          "EACCES",
          `EACCES: permission denied, open '${rawPath}'`,
          "open",
          rawPath
        );
      }
      throw err;
    }
  },
  writeFileSync(file, data, _options) {
    validateEncodingOption(_options);
    if (typeof file === "number") {
      const fd = normalizeFdInteger(file);
      const encoding = typeof _options === "string" ? _options : _options?.encoding;
      const bytes = toUint8ArrayChunk(data, encoding);
      let offset = 0;
      while (offset < bytes.byteLength) {
        offset += fs.writeSync(fd, bytes, offset, bytes.byteLength - offset, null);
      }
      return;
    }
    const rawPath = normalizePathLike(file);
    const pathStr = resolveOperationPath(file);
    try {
      if (typeof data === "string") {
        return _fs.writeFile.applySyncPromise(void 0, [pathStr, data]);
      } else if (ArrayBuffer.isView(data)) {
        const uint8 = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
        if (hasBridgeSyncFn("_fsWriteFileBinaryRaw")) {
          return _fs.writeFileBinaryRaw.applySyncPromise(void 0, [pathStr, uint8]);
        }
        return _fs.writeFileBinary.applySyncPromise(void 0, [pathStr, encodeBridgeBytes(uint8)]);
      } else {
        return _fs.writeFile.applySyncPromise(void 0, [pathStr, String(data)]);
      }
    } catch (err) {
      throwNormalizedFsBridgeError(err, "write", rawPath);
    }
  },
  appendFileSync(path, data, options) {
    validateEncodingOption(options);
    const rawPath = normalizePathLike(path);
    let existing = "";
    try {
      existing = fs.existsSync(path) ? fs.readFileSync(path, "utf8") : "";
    } catch (err) {
      throwNormalizedFsBridgeError(err, "open", rawPath);
    }
    const content = typeof data === "string" ? data : String(data);
    try {
      fs.writeFileSync(path, existing + content, options);
    } catch (err) {
      if (!err?.code) {
        throw createFsError("EACCES", `EACCES: permission denied, write '${rawPath}'`, "write", rawPath);
      }
      throwNormalizedFsBridgeError(err, "write", rawPath);
    }
  },
  readdirSync(path, options) {
    validateEncodingOption(options);
    const rawPath = normalizePathLike(path);
    const pathStr = resolveOperationPath(path);
    let entries;
    try {
      entries = _fs.readDir.applySyncPromise(void 0, [pathStr]);
    } catch (err) {
      if (bridgeErrorCode(err) === "ENOENT") {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, scandir '${rawPath}'`,
          "scandir",
          rawPath
        );
      }
      throw err;
    }
    if (entries instanceof Uint8Array) {
      return decodeRawReaddirEntries(entries, rawPath, options?.withFileTypes);
    }
    return normalizeReaddirEntries(decodeBridgeJson(entries), rawPath, options?.withFileTypes);
  },
  mkdirSync(path, options) {
    const rawPath = normalizePathLike(path);
    const pathStr = rawPath;
    const recursive = typeof options === "object" ? options?.recursive ?? false : false;
    const rawMode = typeof options === "object" ? options?.mode : options;
    const normalizedMode = rawMode === void 0 ? void 0 : normalizeModeArgument(rawMode);
    _fs.mkdir.applySyncPromise(void 0, [pathStr, {
      recursive,
      mode: applyProcessUmask(normalizedMode ?? 511)
    }]);
    return recursive ? rawPath : void 0;
  },
  rmdirSync(path, _options) {
    const pathStr = normalizePathLike(path);
    _fs.rmdir.applySyncPromise(void 0, [pathStr]);
  },
  rmSync(path, options) {
    const pathStr = toPathString(path);
    const opts = options || {};
    try {
      const stats = fs.statSync(pathStr);
      if (stats.isDirectory()) {
        if (opts.recursive) {
          const entries = fs.readdirSync(pathStr);
          for (const entry of entries) {
            const entryPath = pathStr.endsWith("/") ? pathStr + entry : pathStr + "/" + entry;
            const entryStats = fs.statSync(entryPath);
            if (entryStats.isDirectory()) {
              fs.rmSync(entryPath, { recursive: true });
            } else {
              fs.unlinkSync(entryPath);
            }
          }
          fs.rmdirSync(pathStr);
        } else {
          fs.rmdirSync(pathStr);
        }
      } else {
        fs.unlinkSync(pathStr);
      }
    } catch (e) {
      if (opts.force && e.code === "ENOENT") {
        return;
      }
      throw e;
    }
  },
  existsSync(path) {
    const rawPath = tryNormalizeExistsPath(path);
    if (!rawPath) {
      return false;
    }
    const pathStr = resolveOperationPath(rawPath);
    // NOTE: residual band-aid. The kernel device layer + permission exemption
    // (is_standard_device_path) now serve readFileSync/statSync on /dev/null
    // through the host fs path, but `_fs.exists` ("fs.existsSync") still returns
    // false for standard devices — the host exists path swallows the lookup via
    // PermissionedFileSystem::exists' error->Ok(false) branch. Until that exists
    // path is fixed to honor the device layer like read/stat do, keep this guard
    // so existsSync("/dev/null") matches native Linux. See
    // ~/.agents/research/v8-bridge-shim-analysis.md.
    if (
      pathStr === "/dev/null" ||
      pathStr === "/dev/zero" ||
      pathStr === "/dev/urandom" ||
      pathStr === "/dev/stdin" ||
      pathStr === "/dev/stdout" ||
      pathStr === "/dev/stderr"
    ) {
      return true;
    }
    // Node's existsSync() is deliberately non-throwing for filesystem lookup
    // failures (including ENAMETOOLONG). Consumers commonly probe either a
    // literal value or a path, so preserve that contract across the bridge.
    try {
      return Boolean(_fs.exists.applySyncPromise(void 0, [pathStr]));
    } catch {
      return false;
    }
  },
  statSync(path, _options) {
    const rawPath = normalizePathLike(path);
    const pathStr = resolveOperationPath(path);
    let statJson;
    try {
      statJson = _fs.stat.applySyncPromise(void 0, [pathStr]);
    } catch (err) {
      if (bridgeErrorCode(err) === "ENOENT") {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, stat '${rawPath}'`,
          "stat",
          rawPath
        );
      }
      throw err;
    }
    const stat = decodeBridgeJson(statJson);
    return new Stats(stat);
  },
  lstatSync(path, _options) {
    const pathStr = normalizePathLike(path);
    const statJson = bridgeCall(() => _fs.lstat.applySyncPromise(void 0, [pathStr]), "lstat", pathStr);
    const stat = decodeBridgeJson(statJson);
    return new Stats(stat);
  },
  unlinkSync(path) {
    const pathStr = normalizePathLike(path);
    _fs.unlink.applySyncPromise(void 0, [pathStr]);
  },
  renameSync(oldPath, newPath) {
    const oldPathStr = resolveOperationPath(normalizePathLike(oldPath, "oldPath"));
    const newPathStr = resolveOperationPath(normalizePathLike(newPath, "newPath"));
    _fs.rename.applySyncPromise(void 0, [oldPathStr, newPathStr]);
  },
  copyFileSync(src, dest, _mode) {
    const content = fs.readFileSync(src);
    fs.writeFileSync(dest, content);
  },
  // Recursive copy
  cpSync(src, dest, options) {
    const srcPath = toPathString(src);
    const destPath = toPathString(dest);
    const opts = options || {};
    const srcStat = fs.statSync(srcPath);
    if (srcStat.isDirectory()) {
      if (!opts.recursive) {
        throw createFsError(
          "ERR_FS_EISDIR",
          `Path is a directory: cp '${srcPath}'`,
          "cp",
          srcPath
        );
      }
      try {
        fs.mkdirSync(destPath, { recursive: true });
      } catch {
      }
      const entries = fs.readdirSync(srcPath);
      for (const entry of entries) {
        const srcEntry = srcPath.endsWith("/") ? srcPath + entry : srcPath + "/" + entry;
        const destEntry = destPath.endsWith("/") ? destPath + entry : destPath + "/" + entry;
        fs.cpSync(srcEntry, destEntry, opts);
      }
    } else {
      if (opts.errorOnExist && fs.existsSync(destPath)) {
        throw createFsError(
          "EEXIST",
          `EEXIST: file already exists, cp '${srcPath}' -> '${destPath}'`,
          "cp",
          destPath
        );
      }
      if (!opts.force && opts.force !== void 0 && fs.existsSync(destPath)) {
        return;
      }
      fs.copyFileSync(srcPath, destPath);
    }
  },
  // Temp directory creation
  mkdtempSync(prefix, _options) {
    validateEncodingOption(_options);
    const prefixPath = normalizePathLike(prefix, "prefix");
    const charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    for (let attempt = 0; attempt < 10; attempt += 1) {
      const entropy = builtinCryptoModule.randomBytes(6);
      let suffix = "";
      for (const value of entropy) {
        suffix += charset[value % charset.length];
      }
      const dirPath = prefixPath + suffix;
      try {
        bridgeCall(() => _fs.mkdir.applySyncPromise(void 0, [dirPath, {
          recursive: false,
          mode: applyProcessUmask(511)
        }]), "mkdir", dirPath);
        return dirPath;
      } catch (error) {
        if (attempt < 9 && (error?.code === "EEXIST" || bridgeErrorCode(error) === "EEXIST")) {
          continue;
        }
        throw error;
      }
    }
    throw createFsError(
      "EEXIST",
      `EEXIST: file already exists, mkdtemp '${prefixPath}'`,
      "mkdtemp",
      prefixPath
    );
  },
  // Directory handle (sync)
  opendirSync(path, _options) {
    const pathStr = normalizePathLike(path);
    const stat = fs.statSync(pathStr);
    if (!stat.isDirectory()) {
      throw createFsError(
        "ENOTDIR",
        `ENOTDIR: not a directory, opendir '${pathStr}'`,
        "opendir",
        pathStr
      );
    }
    return new Dir(pathStr);
  },
  // File descriptor methods
  openSync(path, flags, _mode) {
    const pathStr = resolveOperationPath(path);
    const numFlags = parseFlags(flags ?? "r");
    const requestedMode = normalizeOpenModeArgument(_mode);
    const modeNum = numFlags & O_CREAT ? applyProcessUmask(requestedMode ?? 438) : requestedMode;
    try {
      return _fdOpen.applySyncPromise(void 0, [pathStr, numFlags, modeNum]);
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("ENOENT")) throw createFsError("ENOENT", msg, "open", pathStr);
      if (msg.includes("EMFILE")) throw createFsError("EMFILE", msg, "open", pathStr);
      if (bridgeErrorCode(e) === "ENXIO") {
        throw createFsError("ENXIO", msg, "open", pathStr);
      }
      throw e;
    }
  },
  closeSync(fd) {
    normalizeFdInteger(fd);
    // If this fd is still bound to a live child's inherited stdio, defer the
    // actual close until the child exits (node keeps it open for the child).
    if (deferCloseIfChildInheritedFd(fd)) {
      return;
    }
    try {
      _fdClose.applySyncPromise(void 0, [fd]);
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, close", "close");
      throw e;
    }
  },
  readSync(fd, buffer, offset, length, position) {
    const normalized = normalizeReadSyncArgs(buffer, offset, length, position);
    const bridgeReadLength = Math.min(normalized.length, MAX_SYNC_BRIDGE_READ_BYTES);
    let bytes;
    try {
      if (hasBridgeSyncFn("_fsReadRaw")) {
        const rawBytes = _fsReadRaw.applySyncPromise(void 0, [fd, bridgeReadLength, normalized.position ?? null]);
        bytes = rawBytes instanceof Uint8Array ? rawBytes : import_buffer.Buffer.from(rawBytes);
      } else {
        const base64 = _fdRead.applySyncPromise(void 0, [fd, bridgeReadLength, normalized.position ?? null]);
        bytes = import_buffer.Buffer.from(base64, "base64");
      }
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) {
        throw createFsError("EBADF", msg, "read");
      }
      throw e;
    }
    const targetBuffer = new Uint8Array(
      normalized.buffer.buffer,
      normalized.buffer.byteOffset,
      normalized.buffer.byteLength
    );
    const bytesRead = Math.min(bytes.length, normalized.length);
    targetBuffer.set(bytes.subarray(0, bytesRead), normalized.offset);
    return bytesRead;
  },
  writeSync(fd, buffer, offsetOrPosition, lengthOrEncoding, position) {
    const normalized = normalizeWriteSyncArgs(buffer, offsetOrPosition, lengthOrEncoding, position);
    let dataBytes;
    if (typeof normalized.buffer === "string") {
      dataBytes = import_buffer.Buffer.from(normalized.buffer, normalized.encoding);
    } else {
      dataBytes = new Uint8Array(
        normalized.buffer.buffer,
        normalized.buffer.byteOffset + normalized.offset,
        normalized.length
      );
    }
    const pos = normalized.position ?? null;
    try {
      if (hasBridgeSyncFn("_fsWriteRaw")) {
        return _fsWriteRaw.applySyncPromise(void 0, [fd, dataBytes, pos]);
      }
      return _fdWrite.applySyncPromise(void 0, [fd, encodeBridgeBytes(dataBytes), pos]);
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) {
        throw createFsError("EBADF", msg, "write");
      }
      throw e;
    }
  },
  fstatSync(fd) {
    normalizeFdInteger(fd);
    let raw;
    try {
      raw = _fdFstat.applySyncPromise(void 0, [fd]);
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fstat", "fstat");
      throw e;
    }
    return new Stats(decodeBridgeJson(raw));
  },
  ftruncateSync(fd, len) {
    normalizeFdInteger(fd);
    try {
      _fdFtruncate.applySyncPromise(void 0, [fd, len]);
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, ftruncate", "ftruncate");
      throw e;
    }
  },
  // fsync / fdatasync — no-op for in-memory VFS (validates FD exists)
  fsyncSync(fd) {
    normalizeFdInteger(fd);
    try {
      _fdFsync.applySyncPromise(void 0, [fd]);
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fsync", "fsync");
      throw e;
    }
  },
  fdatasyncSync(fd) {
    normalizeFdInteger(fd);
    try {
      _fdFsync.applySyncPromise(void 0, [fd]);
    } catch (e) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fdatasync", "fdatasync");
      throw e;
    }
  },
  // readv — scatter-read into multiple buffers (delegates to readSync)
  readvSync(fd, buffers, position) {
    const normalizedFd = normalizeFdInteger(fd);
    const normalizedBuffers = normalizeIoVectorBuffers(buffers);
    let totalBytesRead = 0;
    const normalizedPosition = normalizeOptionalPosition(position);
    let nextPosition = normalizedPosition;
    for (const buffer of normalizedBuffers) {
      const target = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
      const bytesRead = fs.readSync(normalizedFd, target, 0, target.byteLength, nextPosition);
      totalBytesRead += bytesRead;
      if (nextPosition !== null) {
        nextPosition += bytesRead;
      }
      if (bytesRead < target.byteLength) break;
    }
    return totalBytesRead;
  },
  // statfs — return synthetic filesystem stats for the in-memory VFS
  statfsSync(path, _options) {
    const pathStr = normalizePathLike(path);
    if (!fs.existsSync(pathStr)) {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, statfs '${pathStr}'`,
        "statfs",
        pathStr
      );
    }
    return {
      type: 16914839,
      // TMPFS_MAGIC
      bsize: 4096,
      blocks: 262144,
      // 1GB virtual capacity
      bfree: 262144,
      bavail: 262144,
      files: 1e6,
      ffree: 999999
    };
  },
  // glob — pattern matching over VFS files
  globSync(pattern, _options) {
    const patterns = Array.isArray(pattern) ? pattern : [pattern];
    const rawOptions = _options && typeof _options === "object" ? _options : {};
    const cwd = normalizePathLike(rawOptions.cwd ?? globalThis.process?.cwd?.() ?? ".", "options.cwd");
    const exclude = rawOptions.exclude;
    if (exclude !== void 0 && typeof exclude !== "function" && !Array.isArray(exclude)) {
      throw createInvalidArgTypeError("options.exclude", "of type function or an Array", exclude);
    }
    const options = {
      cwd,
      exclude,
      excludeRegexes: Array.isArray(exclude) ? exclude.map((value) => _globToRegex(normalizePathLike(value, "options.exclude"))) : [],
      withFileTypes: rawOptions.withFileTypes === true
    };
    const results = /* @__PURE__ */ new Map();
    for (const pat of patterns) {
      _globCollect(normalizePathLike(pat, "pattern"), options, results);
    }
    return [...results.entries()].sort(([left], [right]) => left.localeCompare(right)).map(([, value]) => value);
  },
  // Metadata and link sync methods — delegate to VFS via host refs
  chmodSync(path, mode) {
    const pathStr = normalizePathLike(path);
    const modeNum = normalizeModeArgument(mode);
    bridgeCall(() => _fs.chmod.applySyncPromise(void 0, [pathStr, modeNum]), "chmod", pathStr);
  },
  chownSync(path, uid, gid) {
    const pathStr = normalizePathLike(path);
    const normalizedUid = normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
    const normalizedGid = normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
    bridgeCall(() => _fs.chown.applySyncPromise(void 0, [pathStr, normalizedUid, normalizedGid]), "chown", pathStr);
  },
  fchmodSync(fd, mode) {
    const normalizedFd = normalizeFdInteger(fd);
    const pathStr = _fdGetPath.applySync(void 0, [normalizedFd]);
    if (!pathStr) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chmod");
    }
    fs.chmodSync(pathStr, normalizeModeArgument(mode));
  },
  fchownSync(fd, uid, gid) {
    const normalizedFd = normalizeFdInteger(fd);
    const pathStr = _fdGetPath.applySync(void 0, [normalizedFd]);
    if (!pathStr) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chown");
    }
    fs.chownSync(pathStr, uid, gid);
  },
  lchownSync(path, uid, gid) {
    const pathStr = normalizePathLike(path);
    const normalizedUid = normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
    const normalizedGid = normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
    bridgeCall(() => _fs.chown.applySyncPromise(void 0, [pathStr, normalizedUid, normalizedGid]), "chown", pathStr);
  },
  linkSync(existingPath, newPath) {
    const existingStr = normalizePathLike(existingPath, "existingPath");
    const newStr = normalizePathLike(newPath, "newPath");
    bridgeCall(() => _fs.link.applySyncPromise(void 0, [existingStr, newStr]), "link", newStr);
  },
  symlinkSync(target, path, _type) {
    const targetStr = normalizePathLike(target, "target");
    const pathStr = normalizePathLike(path);
    bridgeCall(() => _fs.symlink.applySyncPromise(void 0, [targetStr, pathStr]), "symlink", pathStr);
  },
  readlinkSync(path, _options) {
    validateEncodingOption(_options);
    const pathStr = normalizePathLike(path);
    return bridgeCall(() => _fs.readlink.applySyncPromise(void 0, [pathStr]), "readlink", pathStr);
  },
  truncateSync(path, len) {
    const pathStr = normalizePathLike(path);
    bridgeCall(() => _fs.truncate.applySyncPromise(void 0, [pathStr, len ?? 0]), "truncate", pathStr);
  },
  utimesSync(path, atime, mtime) {
    const pathStr = normalizePathLike(path);
    bridgeCall(() => _fs.utimes.applySyncPromise(void 0, [
      pathStr,
      normalizeFsTimeSpec(atime, "atime"),
      normalizeFsTimeSpec(mtime, "mtime")
    ]), "utimes", pathStr);
  },
  lutimesSync(path, atime, mtime) {
    const pathStr = normalizePathLike(path);
    bridgeCall(() => _fs.lutimes.applySyncPromise(void 0, [
      pathStr,
      normalizeFsTimeSpec(atime, "atime"),
      normalizeFsTimeSpec(mtime, "mtime")
    ]), "lutimes", pathStr);
  },
  futimesSync(fd, atime, mtime) {
    const normalizedFd = normalizeFdInteger(fd);
    bridgeCall(() => _fdFutimes.applySyncPromise(void 0, [
      normalizedFd,
      normalizeFsTimeSpec(atime, "atime"),
      normalizeFsTimeSpec(mtime, "mtime")
    ]), "futimes");
  },
  // Async methods - wrap sync methods in callbacks/promises
  //
  // IMPORTANT: Low-level fd operations (open, close, read, write) and operations commonly
  // used by streaming libraries (stat, lstat, rename, unlink) must defer their callbacks
  // using queueMicrotask(). This is critical for proper stream operation.
  //
  // Why: Node.js streams (like tar, minipass, fs-minipass) use callback chains where each
  // callback triggers the next read/write operation. These streams also rely on events like
  // 'drain' to know when to resume writing. If callbacks fire synchronously, the event loop
  // never gets a chance to process these events, causing streams to stall after the first chunk.
  //
  // Example problem without queueMicrotask:
  //   1. tar calls fs.read() with callback
  //   2. Our sync implementation calls callback immediately
  //   3. Callback writes to stream, stream buffer fills, returns false (needs drain)
  //   4. Code sets up 'drain' listener and returns
  //   5. But we never returned to event loop, so 'drain' never fires
  //   6. Stream hangs forever
  //
  // With queueMicrotask, step 2 defers the callback, allowing the event loop to process
  // pending events (including 'drain') before the next operation starts.
  readFile(path, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      validateCallback(callback);
      fsReadFileAsync(path, options).then(
        (data) => callback(null, data),
        (error) => callback(error)
      );
    } else {
      return Promise.resolve(fs.readFileSync(path, options));
    }
  },
  writeFile(path, data, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(options);
      try {
        fs.writeFileSync(path, data, options);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(
        fs.writeFileSync(path, data, options)
      );
    }
  },
  appendFile(path, data, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(options);
      try {
        fs.appendFileSync(path, data, options);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(
        fs.appendFileSync(path, data, options)
      );
    }
  },
  readdir(path, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(options);
      try {
        callback(null, fs.readdirSync(path, options));
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(
        fs.readdirSync(path, options)
      );
    }
  },
  mkdir(path, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      normalizePathLike(path);
      try {
        fs.mkdirSync(path, options);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      fs.mkdirSync(path, options);
      return Promise.resolve();
    }
  },
  rmdir(path, callback) {
    if (callback) {
      normalizePathLike(path);
      const cb = callback;
      try {
        fs.rmdirSync(path);
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    } else {
      return Promise.resolve(fs.rmdirSync(path));
    }
  },
  // rm - remove files or directories (with recursive support)
  rm(path, options, callback) {
    let opts = {};
    let cb;
    if (typeof options === "function") {
      cb = options;
    } else if (options) {
      opts = options;
      cb = callback;
    } else {
      cb = callback;
    }
    const doRm = () => {
      try {
        const stats = fs.statSync(path);
        if (stats.isDirectory()) {
          if (opts.recursive) {
            const entries = fs.readdirSync(path);
            for (const entry of entries) {
              const entryPath = path.endsWith("/") ? path + entry : path + "/" + entry;
              const entryStats = fs.statSync(entryPath);
              if (entryStats.isDirectory()) {
                fs.rmSync(entryPath, { recursive: true });
              } else {
                fs.unlinkSync(entryPath);
              }
            }
            fs.rmdirSync(path);
          } else {
            fs.rmdirSync(path);
          }
        } else {
          fs.unlinkSync(path);
        }
      } catch (e) {
        if (opts.force && e.code === "ENOENT") {
          return;
        }
        throw e;
      }
    };
    if (cb) {
      try {
        doRm();
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    } else {
      doRm();
      return Promise.resolve();
    }
  },
  exists(path, callback) {
    validateCallback(callback, "cb");
    if (path === void 0) {
      throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", path);
    }
    queueMicrotask(() => callback(Boolean(tryNormalizeExistsPath(path) && fs.existsSync(path))));
  },
  stat(path, callback) {
    validateCallback(callback, "cb");
    normalizePathLike(path);
    const cb = callback;
    try {
      const stats = fs.statSync(path);
      queueMicrotask(() => cb(null, stats));
    } catch (e) {
      queueMicrotask(() => cb(e));
    }
  },
  lstat(path, callback) {
    if (callback) {
      const cb = callback;
      try {
        const stats = fs.lstatSync(path);
        queueMicrotask(() => cb(null, stats));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    } else {
      return Promise.resolve(fs.lstatSync(path));
    }
  },
  unlink(path, callback) {
    if (callback) {
      normalizePathLike(path);
      const cb = callback;
      try {
        fs.unlinkSync(path);
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    } else {
      return Promise.resolve(fs.unlinkSync(path));
    }
  },
  rename(oldPath, newPath, callback) {
    if (callback) {
      normalizePathLike(oldPath, "oldPath");
      normalizePathLike(newPath, "newPath");
      const cb = callback;
      try {
        fs.renameSync(oldPath, newPath);
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    } else {
      return Promise.resolve(fs.renameSync(oldPath, newPath));
    }
  },
  copyFile(src, dest, callback) {
    if (callback) {
      try {
        fs.copyFileSync(src, dest);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.copyFileSync(src, dest));
    }
  },
  cp(src, dest, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      try {
        fs.cpSync(src, dest, options);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.cpSync(src, dest, options));
    }
  },
  mkdtemp(prefix, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    validateCallback(callback, "cb");
    validateEncodingOption(options);
    try {
      callback(null, fs.mkdtempSync(prefix, options));
    } catch (e) {
      callback(e);
    }
  },
  opendir(path, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      try {
        callback(null, fs.opendirSync(path, options));
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.opendirSync(path, options));
    }
  },
  open(path, flags, mode, callback) {
    let resolvedFlags = "r";
    let resolvedMode = mode;
    if (typeof flags === "function") {
      callback = flags;
      resolvedMode = void 0;
    } else {
      resolvedFlags = flags ?? "r";
    }
    if (typeof mode === "function") {
      callback = mode;
      resolvedMode = void 0;
    }
    validateCallback(callback, "cb");
    normalizePathLike(path);
    normalizeOpenModeArgument(resolvedMode);
    const cb = callback;
    try {
      const fd = fs.openSync(path, resolvedFlags, resolvedMode);
      queueMicrotask(() => cb(null, fd));
    } catch (e) {
      queueMicrotask(() => cb(e));
    }
  },
  close(fd, callback) {
    normalizeFdInteger(fd);
    validateCallback(callback, "cb");
    const cb = callback;
    try {
      fs.closeSync(fd);
      queueMicrotask(() => cb(null));
    } catch (e) {
      queueMicrotask(() => cb(e));
    }
  },
  read(fd, buffer, offset, length, position, callback) {
    // Node also supports read(fd, options, callback) and read(fd, callback).
    // Effect's platform filesystem uses the options form for readAlloc().
    if (typeof buffer === "function") {
      callback = buffer;
      buffer = import_buffer.Buffer.alloc(FILE_HANDLE_READ_BUFFER_BYTES);
      offset = 0;
      length = buffer.byteLength;
      position = null;
    } else if (
      buffer !== null &&
      typeof buffer === "object" &&
      !ArrayBuffer.isView(buffer)
    ) {
      const options = buffer;
      callback = typeof offset === "function" ? offset : callback;
      buffer = options.buffer ?? import_buffer.Buffer.alloc(FILE_HANDLE_READ_BUFFER_BYTES);
      offset = options.offset ?? 0;
      length = options.length ?? buffer.byteLength - offset;
      position = options.position ?? null;
    }
    if (callback) {
      validateCallback(callback);
      const cb = callback;
      if (fd === 0 && (position === null || position === void 0) && typeof _kernelStdinRead !== "undefined") {
        const target = new Uint8Array(buffer.buffer, buffer.byteOffset + offset, length);
        _kernelStdinRead.apply(void 0, [length, null], {
          result: { promise: true }
        }).then((next) => {
            if (next == null) {
              queueMicrotask(() => cb(createFsError("EAGAIN", "EAGAIN: stdin readiness wait returned without data", "read")));
              return;
            }
            if (next?.done) {
              queueMicrotask(() => cb(null, 0, buffer));
              return;
            }
            const dataBase64 = String(next?.dataBase64 ?? "");
            if (!dataBase64) {
              queueMicrotask(() => cb(createFsError("EAGAIN", "EAGAIN: stdin readiness wait returned an empty payload", "read")));
              return;
            }
            const bytes = import_buffer.Buffer.from(dataBase64, "base64");
            const bytesRead = Math.min(length, bytes.length);
            target.set(bytes.subarray(0, bytesRead), 0);
            queueMicrotask(() => cb(null, bytesRead, buffer));
          }, (error) => {
            queueMicrotask(() => cb(error));
          });
        return;
      }
      const attemptRead = () => {
        try {
          const bytesRead = fs.readSync(fd, buffer, offset, length, position);
          queueMicrotask(() => cb(null, bytesRead, buffer));
        } catch (e) {
          const msg = e?.message ?? String(e);
          if (msg.includes("EAGAIN") && hasBridgeAsyncFn("_kernelPoll")) {
            _kernelPoll.apply(void 0, [[{ fd, events: KERNEL_POLLIN }], null]).then(
              () => attemptRead(),
              (error) => queueMicrotask(() => cb(error))
            );
            return;
          }
          queueMicrotask(() => cb(e));
        }
      };
      attemptRead();
    } else {
      return Promise.resolve(fs.readSync(fd, buffer, offset, length, position));
    }
  },
  write(fd, buffer, offset, length, position, callback) {
    if (typeof offset === "function") {
      callback = offset;
      offset = void 0;
      length = void 0;
      position = void 0;
    } else if (typeof length === "function") {
      callback = length;
      length = void 0;
      position = void 0;
    } else if (typeof position === "function") {
      callback = position;
      position = void 0;
    }
    if (callback) {
      const normalized = normalizeWriteSyncArgs(
        buffer,
        offset,
        length,
        position
      );
      const cb = callback;
      try {
        const bytesWritten = fs.writeSync(
          fd,
          buffer,
          offset,
          length,
          position
        );
        queueMicrotask(() => cb(null, bytesWritten));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    } else {
      return Promise.resolve(
        fs.writeSync(
          fd,
          buffer,
          offset,
          length,
          position
        )
      );
    }
  },
  // writev - write multiple buffers to a file descriptor
  writev(fd, buffers, position, callback) {
    if (typeof position === "function") {
      callback = position;
      position = null;
    }
    const normalizedFd = normalizeFdInteger(fd);
    const normalizedBuffers = normalizeIoVectorBuffers(buffers);
    const normalizedPosition = normalizeOptionalPosition(position);
    if (callback) {
      try {
        const bytesWritten = fs.writevSync(normalizedFd, normalizedBuffers, normalizedPosition);
        queueMicrotask(() => callback(null, bytesWritten, normalizedBuffers));
      } catch (e) {
        queueMicrotask(() => callback(e));
      }
    } else {
      return Promise.resolve(fs.writevSync(normalizedFd, normalizedBuffers, normalizedPosition));
    }
  },
  writevSync(fd, buffers, position) {
    const normalizedFd = normalizeFdInteger(fd);
    const normalizedBuffers = normalizeIoVectorBuffers(buffers);
    if (hasBridgeSyncFn("_fsWritevRaw")) {
      const normalizedPosition = normalizeOptionalPosition(position);
      const payload = encodeWritevRawPayload(normalizedBuffers);
      return _fsWritevRaw.applySyncPromise(void 0, [normalizedFd, payload, normalizedPosition]);
    }
    let nextPosition = normalizeOptionalPosition(position);
    let totalBytesWritten = 0;
    for (const buffer of normalizedBuffers) {
      const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
      totalBytesWritten += fs.writeSync(normalizedFd, bytes, 0, bytes.length, nextPosition);
      if (nextPosition !== null) {
        nextPosition += bytes.length;
      }
    }
    return totalBytesWritten;
  },
  fstat(fd, callback) {
    if (callback) {
      try {
        callback(null, fs.fstatSync(fd));
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.fstatSync(fd));
    }
  },
  // fsync / fdatasync async callback forms
  fsync(fd, callback) {
    normalizeFdInteger(fd);
    validateCallback(callback, "cb");
    try {
      fs.fsyncSync(fd);
      callback(null);
    } catch (e) {
      callback(e);
    }
  },
  fdatasync(fd, callback) {
    normalizeFdInteger(fd);
    validateCallback(callback, "cb");
    try {
      fs.fdatasyncSync(fd);
      callback(null);
    } catch (e) {
      callback(e);
    }
  },
  // readv async callback form
  readv(fd, buffers, position, callback) {
    if (typeof position === "function") {
      callback = position;
      position = null;
    }
    const normalizedFd = normalizeFdInteger(fd);
    const normalizedBuffers = normalizeIoVectorBuffers(buffers);
    const normalizedPosition = normalizeOptionalPosition(position);
    if (callback) {
      try {
        const bytesRead = fs.readvSync(normalizedFd, normalizedBuffers, normalizedPosition);
        queueMicrotask(() => callback(null, bytesRead, normalizedBuffers));
      } catch (e) {
        queueMicrotask(() => callback(e));
      }
    }
  },
  // statfs async callback form
  statfs(path, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      try {
        callback(null, fs.statfsSync(path, options));
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.statfsSync(path, options));
    }
  },
  // glob async callback form
  glob(pattern, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = void 0;
    }
    if (callback) {
      try {
        callback(null, fs.globSync(pattern, options));
      } catch (e) {
        callback(e);
      }
    }
  },
  // fs.promises API
  // Note: Using async functions to properly catch sync errors and return rejected promises
  promises: {
    async readFile(path, options) {
      if (path instanceof FileHandle) {
        return path.readFile(options);
      }
      return fsReadFileAsync(path, options);
    },
    async writeFile(path, data, options) {
      if (path instanceof FileHandle) {
        return path.writeFile(data, options);
      }
      return fsWriteFileAsync(path, data, options);
    },
    async appendFile(path, data, options) {
      if (path instanceof FileHandle) {
        return path.appendFile(data, options);
      }
      const existing = await fsReadFileAsync(path, "utf8").catch((err) => err?.code === "ENOENT" ? "" : Promise.reject(err));
      const content = typeof data === "string" ? data : String(data);
      await fsWriteFileAsync(path, existing + content, options);
    },
    async readdir(path, options) {
      return fsReaddirAsync(path, options);
    },
    async mkdir(path, options) {
      return fsMkdirAsync(path, options);
    },
    async rmdir(path) {
      return fsRmdirAsync(path);
    },
    async stat(path) {
      return fsStatAsync(path);
    },
    async lstat(path) {
      return fsLstatAsync(path);
    },
    async unlink(path) {
      return fsUnlinkAsync(path);
    },
    async rename(oldPath, newPath) {
      return fsRenameAsync(oldPath, newPath);
    },
    async copyFile(src, dest) {
      const content = await fsReadFileAsync(src);
      await fsWriteFileAsync(dest, content);
    },
    async cp(src, dest, options) {
      return fs.cpSync(src, dest, options);
    },
    async mkdtemp(prefix, options) {
      return fs.mkdtempSync(prefix, options);
    },
    async opendir(path, options) {
      return fs.opendirSync(path, options);
    },
    async open(path, flags, mode) {
      return new FileHandle(fs.openSync(path, flags ?? "r", mode));
    },
    async statfs(path, options) {
      return fs.statfsSync(path, options);
    },
    async glob(pattern, _options) {
      return fs.globSync(pattern, _options);
    },
    async access(path) {
      return fsAccessAsync(path);
    },
    async rm(path, options) {
      return fs.rmSync(path, options);
    },
    async chmod(path, mode) {
      return fsChmodAsync(path, mode);
    },
    async chown(path, uid, gid) {
      return fsChownAsync(path, uid, gid);
    },
    async lchown(path, uid, gid) {
      return fs.lchownSync(path, uid, gid);
    },
    async lutimes(path, atime, mtime) {
      return fsLutimesAsync(path, atime, mtime);
    },
    async link(existingPath, newPath) {
      return fsLinkAsync(existingPath, newPath);
    },
    async symlink(target, path) {
      return fsSymlinkAsync(target, path);
    },
    async readlink(path) {
      return fsReadlinkAsync(path);
    },
    async realpath(path, options) {
      return fs.realpathSync(path, options);
    },
    async truncate(path, len) {
      return fsTruncateAsync(path, len);
    },
    async utimes(path, atime, mtime) {
      return fsUtimesAsync(path, atime, mtime);
    },
    watch(path, options) {
      return createPromisesWatchIterator(path, options);
    }
  },
  // Compatibility methods
  accessSync(path) {
    if (!fs.existsSync(path)) {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, access '${path}'`,
        "access",
        path
      );
    }
  },
  access(path, mode, callback) {
    if (typeof mode === "function") {
      callback = mode;
      mode = void 0;
    }
    if (callback) {
      try {
        fs.accessSync(path);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return fs.promises.access(path);
    }
  },
  realpathSync: Object.assign(
    function realpathSync(path, options) {
      validateEncodingOption(options);
      const MAX_SYMLINK_DEPTH = 40;
      let symlinksFollowed = 0;
      const raw = normalizePathLike(path);
      const pending = [];
      for (const seg of raw.split("/")) {
        if (!seg || seg === ".") continue;
        if (seg === "..") {
          if (pending.length > 0) pending.pop();
        } else pending.push(seg);
      }
      const resolved = [];
      while (pending.length > 0) {
        const seg = pending.shift();
        if (seg === ".") continue;
        if (seg === "..") {
          if (resolved.length > 0) resolved.pop();
          continue;
        }
        resolved.push(seg);
        const currentPath = "/" + resolved.join("/");
        try {
          const stat = fs.lstatSync(currentPath);
          if (stat.isSymbolicLink()) {
            if (++symlinksFollowed > MAX_SYMLINK_DEPTH) {
              const err = new Error(`ELOOP: too many levels of symbolic links, realpath '${raw}'`);
              err.code = "ELOOP";
              err.syscall = "realpath";
              err.path = raw;
              throw err;
            }
            const target = fs.readlinkSync(currentPath);
            const targetSegs = target.split("/").filter(Boolean);
            if (target.startsWith("/")) {
              resolved.length = 0;
            } else {
              resolved.pop();
            }
            pending.unshift(...targetSegs);
          }
        } catch (e) {
          const err = e;
          if (err.code === "ELOOP") throw e;
          if (err.code === "ENOENT" || err.code === "ENOTDIR") {
            const enoent = new Error(`ENOENT: no such file or directory, realpath '${raw}'`);
            enoent.code = "ENOENT";
            enoent.syscall = "realpath";
            enoent.path = raw;
            throw enoent;
          }
          break;
        }
      }
      return "/" + resolved.join("/") || "/";
    },
    {
      native(path, options) {
        validateEncodingOption(options);
        return fs.realpathSync(path);
      }
    }
  ),
  realpath: Object.assign(
    function realpath(path, optionsOrCallback, callback) {
      let options;
      if (typeof optionsOrCallback === "function") {
        callback = optionsOrCallback;
      } else {
        options = optionsOrCallback;
      }
      if (callback) {
        validateEncodingOption(options);
        callback(null, fs.realpathSync(path, options));
      } else {
        return Promise.resolve(fs.realpathSync(path, options));
      }
    },
    {
      native(path, optionsOrCallback, callback) {
        let options;
        if (typeof optionsOrCallback === "function") {
          callback = optionsOrCallback;
        } else {
          options = optionsOrCallback;
        }
        if (callback) {
          validateEncodingOption(options);
          callback(null, fs.realpathSync.native(path, options));
        } else {
          return Promise.resolve(fs.realpathSync.native(path, options));
        }
      }
    }
  ),
  ReadStream: ReadStreamFactory,
  WriteStream: WriteStreamFactory,
  createReadStream: function createReadStream(path, options) {
    const opts = typeof options === "string" ? { encoding: options } : options;
    validateEncodingOption(opts);
    const fd = normalizeStreamFd(opts?.fd);
    const pathLike = normalizeStreamPath(path, fd);
    return new ReadStream(pathLike, opts);
  },
  createWriteStream: function createWriteStream(path, options) {
    const opts = typeof options === "string" ? { encoding: options } : options;
    validateEncodingOption(opts);
    validateWriteStreamStartOption(opts ?? {});
    const fd = normalizeStreamFd(opts?.fd);
    const pathLike = normalizeStreamPath(path, fd);
    return new WriteStream(pathLike, opts);
  },
  // Watch APIs use guest-side polling over statSync until the kernel grows native notifications.
  watch(...args) {
    const { path, listener, options } = normalizeWatchArguments(args[0], args[1], args[2]);
    const watcher = createFsWatcher(path, options);
    if (listener) {
      watcher.on("change", listener);
    }
    return watcher;
  },
  watchFile(...args) {
    const { path, listener, options } = normalizeWatchFileArguments(args[0], args[1], args[2]);
    return createFsStatWatcher(path, options, listener);
  },
  unwatchFile(...args) {
    const path = normalizePathLike(args[0]);
    const listener = args[1];
    if (listener !== void 0 && typeof listener !== "function") {
      throw createInvalidArgTypeError("listener", "of type function", listener);
    }
    const watchers = activeStatWatchers.get(path);
    if (!watchers) {
      return;
    }
    for (const watcher of [...watchers]) {
      const listeners = watcher._listeners.get("change") ?? [];
      if (listener === void 0 || listeners.some(
        (candidate) => candidate === listener || candidate._originalListener === listener
      )) {
        watcher.close();
      }
    }
  },
  chmod(path, mode, callback) {
    if (callback) {
      normalizePathLike(path);
      normalizeModeArgument(mode);
      try {
        fs.chmodSync(path, mode);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.chmodSync(path, mode));
    }
  },
  chown(path, uid, gid, callback) {
    if (callback) {
      normalizePathLike(path);
      normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
      try {
        fs.chownSync(path, uid, gid);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.chownSync(path, uid, gid));
    }
  },
  fchmod(fd, mode, callback) {
    if (callback) {
      normalizeFdInteger(fd);
      normalizeModeArgument(mode);
      try {
        fs.fchmodSync(fd, mode);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      normalizeFdInteger(fd);
      normalizeModeArgument(mode);
      return Promise.resolve(fs.fchmodSync(fd, mode));
    }
  },
  fchown(fd, uid, gid, callback) {
    if (callback) {
      normalizeFdInteger(fd);
      normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
      try {
        fs.fchownSync(fd, uid, gid);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      normalizeFdInteger(fd);
      normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
      return Promise.resolve(fs.fchownSync(fd, uid, gid));
    }
  },
  lchown(path, uid, gid, callback) {
    if (arguments.length >= 4) {
      validateCallback(callback, "cb");
      normalizePathLike(path);
      normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
      try {
        fs.lchownSync(path, uid, gid);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.lchownSync(path, uid, gid));
    }
  },
  link(existingPath, newPath, callback) {
    if (callback) {
      normalizePathLike(existingPath, "existingPath");
      normalizePathLike(newPath, "newPath");
      try {
        fs.linkSync(existingPath, newPath);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.linkSync(existingPath, newPath));
    }
  },
  symlink(target, path, typeOrCb, callback) {
    if (typeof typeOrCb === "function") {
      callback = typeOrCb;
    }
    if (callback) {
      try {
        fs.symlinkSync(target, path);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.symlinkSync(target, path));
    }
  },
  readlink(path, optionsOrCb, callback) {
    if (typeof optionsOrCb === "function") {
      callback = optionsOrCb;
      optionsOrCb = void 0;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(optionsOrCb);
      try {
        callback(null, fs.readlinkSync(path, optionsOrCb));
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.readlinkSync(path, optionsOrCb));
    }
  },
  truncate(path, lenOrCb, callback) {
    if (typeof lenOrCb === "function") {
      callback = lenOrCb;
      lenOrCb = 0;
    }
    if (callback) {
      try {
        fs.truncateSync(path, lenOrCb);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.truncateSync(path, lenOrCb));
    }
  },
  utimes(path, atime, mtime, callback) {
    if (callback) {
      try {
        fs.utimesSync(path, atime, mtime);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.utimesSync(path, atime, mtime));
    }
  },
  lutimes(path, atime, mtime, callback) {
    if (callback) {
      try {
        fs.lutimesSync(path, atime, mtime);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.lutimesSync(path, atime, mtime));
    }
  },
  futimes(fd, atime, mtime, callback) {
    if (callback) {
      try {
        fs.futimesSync(fd, atime, mtime);
        callback(null);
      } catch (e) {
        callback(e);
      }
    } else {
      return Promise.resolve(fs.futimesSync(fd, atime, mtime));
    }
  }
};
_globReadDir = (dir) => fs.readdirSync(dir, { withFileTypes: true });
_globStat = (path) => fs.statSync(path);
var fs_default = fs;
exposeCustomGlobal("_fsModule", fs_default);
export { import_buffer, O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, O_EXCL, O_TRUNC, O_APPEND, Stats, Dirent, Dir, FILE_HANDLE_READ_CHUNK_BYTES, FILE_HANDLE_READ_BUFFER_BYTES, FILE_HANDLE_MAX_READ_BYTES, createAbortError, validateAbortSignal, throwIfAborted, waitForNextTick, createInternalAssertionError, createOutOfRangeError, formatInvalidArgReceived, createInvalidArgTypeError, createInvalidArgValueError, createInvalidEncodingError, toUint8ArrayChunk, iterateWriteChunks, FileHandle, isArrayBufferView, createInvalidPropertyTypeError, validateCallback, validateEncodingValue, validateEncodingOption, normalizePathLike, tryNormalizeExistsPath, normalizeNumberArgument, normalizeModeArgument, normalizeOpenModeArgument, applyProcessUmask, validateWriteStreamStartOption, validateBooleanOption, validateAbortSignalOption, normalizeWatchOptions, normalizeWatchArguments, normalizeWatchFileArguments, createMissingWatcherStats, createWatcherSnapshot, createWatcherFilename, watcherEventType, DEFAULT_FS_WATCH_INTERVAL_MS, DEFAULT_FS_WATCH_FILE_INTERVAL_MS, activeStatWatchers, PollingFsWatcher, registerStatWatcher, unregisterStatWatcher, createFsWatcher, createFsStatWatcher, createPromisesWatchIterator, isReadWriteOptionsObject, normalizeOptionalPosition, normalizeOffsetLength, normalizeReadSyncArgs, normalizeWriteSyncArgs, normalizeFdInteger, normalizeIoVectorBuffers, validateStreamFsOverride, normalizeStreamFd, normalizeStreamPath, normalizeStreamStartEnd, ReadStream, MAX_WRITE_STREAM_BYTES, WriteStream, ReadStreamClass, WriteStreamClass, ReadStreamFactory, WriteStreamFactory, parseFlags, POSIX_ERRNO, errnoForCode, createFsError, bridgeErrorText, bridgeErrorCode, bridgeCall, _globToRegex, _globGetBase, MAX_GLOB_DEPTH, _globCollect, _globReadDir, _globStat, toPathString, getBridgeSyncFn, createBridgeSyncFacade, createBridgeAsyncFacade, _fs, _fsAsync, _fdOpen, _fdClose, _fdRead, _fdWrite, _fdFstat, _fdFtruncate, _fdFsync, _fdFutimes, _fdGetPath, _processUmask, _processMemoryUsage, _processCpuUsage, _processResourceUsage, _processVersions, _kernelPollRaw, _kernelIsattyRaw, _kernelTtySizeRaw, decodeBridgeJson, encodeBridgeBytes, throwNormalizedFsBridgeError, joinDirEntryPath, normalizeReaddirEntries, fsReadFileAsync, fsWriteFileAsync, fsReaddirAsync, fsMkdirAsync, fsRmdirAsync, fsStatAsync, fsLstatAsync, fsUnlinkAsync, fsRenameAsync, fsAccessAsync, fsChmodAsync, fsChownAsync, fsLinkAsync, fsSymlinkAsync, fsReadlinkAsync, fsTruncateAsync, normalizeFsTimeSpec, fsUtimesAsync, fsLutimesAsync, fs, fs_default };
