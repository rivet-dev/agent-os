// fs polyfill module for the sandbox
// This module runs inside the isolate and provides Node.js fs API compatibility
// It communicates with the host via the _fs Reference object

import { Buffer } from "buffer";
import type * as nodeFs from "fs";
import type { FsFacadeBridge } from "../bridge-contract.js";

// Declare globals that are set up by the host environment
declare const _fs: FsFacadeBridge;

// Kernel FD bridge globals — dispatched through _loadPolyfill on the V8 runtime.
// FD table is managed on the host side via kernel ProcessFDTable.
declare const _fdOpen: { applySync(t: undefined, a: [string, number, number?]): number; applySyncPromise(t: undefined, a: [string, number, number?]): number };
declare const _fdClose: { applySync(t: undefined, a: [number]): void; applySyncPromise(t: undefined, a: [number]): void };
declare const _fdRead: { applySync(t: undefined, a: [number, number, number | null | undefined]): string; applySyncPromise(t: undefined, a: [number, number, number | null | undefined]): string };
declare const _fdWrite: { applySync(t: undefined, a: [number, string, number | null | undefined]): number; applySyncPromise(t: undefined, a: [number, string, number | null | undefined]): number };
declare const _fdFstat: { applySync(t: undefined, a: [number]): string; applySyncPromise(t: undefined, a: [number]): string };
declare const _fdFtruncate: { applySync(t: undefined, a: [number, number?]): void; applySyncPromise(t: undefined, a: [number, number?]): void };
declare const _fdFsync: { applySync(t: undefined, a: [number]): void; applySyncPromise(t: undefined, a: [number]): void };
declare const _fdGetPath: { applySync(t: undefined, a: [number]): string | null; applySyncPromise(t: undefined, a: [number]): string | null };

const O_RDONLY = 0;
const O_WRONLY = 1;
const O_RDWR = 2;
const O_CREAT = 64;
const O_EXCL = 128;
const O_TRUNC = 512;
const O_APPEND = 1024;

// Stats class
class Stats implements nodeFs.Stats {
  dev: number;
  ino: number;
  mode: number;
  nlink: number;
  uid: number;
  gid: number;
  rdev: number;
  size: number;
  blksize: number;
  blocks: number;
  atimeMs: number;
  mtimeMs: number;
  ctimeMs: number;
  birthtimeMs: number;
  atime: Date;
  mtime: Date;
  ctime: Date;
  birthtime: Date;

  constructor(init: {
    dev?: number;
    ino?: number;
    mode: number;
    nlink?: number;
    uid?: number;
    gid?: number;
    rdev?: number;
    size: number;
    blksize?: number;
    blocks?: number;
    atimeMs?: number;
    mtimeMs?: number;
    ctimeMs?: number;
    birthtimeMs?: number;
  }) {
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
    this.atimeMs = init.atimeMs ?? Date.now();
    this.mtimeMs = init.mtimeMs ?? Date.now();
    this.ctimeMs = init.ctimeMs ?? Date.now();
    this.birthtimeMs = init.birthtimeMs ?? Date.now();
    this.atime = new Date(this.atimeMs);
    this.mtime = new Date(this.mtimeMs);
    this.ctime = new Date(this.ctimeMs);
    this.birthtime = new Date(this.birthtimeMs);
  }

  isFile(): boolean {
    return (this.mode & 61440) === 32768;
  }
  isDirectory(): boolean {
    return (this.mode & 61440) === 16384;
  }
  isSymbolicLink(): boolean {
    return (this.mode & 61440) === 40960;
  }
  isBlockDevice(): boolean {
    return false;
  }
  isCharacterDevice(): boolean {
    return false;
  }
  isFIFO(): boolean {
    return false;
  }
  isSocket(): boolean {
    return false;
  }
}

// Dirent class for readdir with withFileTypes
class Dirent implements nodeFs.Dirent<string> {
  name: string;
  parentPath: string;
  path: string; // Deprecated alias for parentPath
  private _isDir: boolean;

  constructor(name: string, isDir: boolean, parentPath: string = "") {
    this.name = name;
    this._isDir = isDir;
    this.parentPath = parentPath;
    this.path = parentPath;
  }

  isFile(): boolean {
    return !this._isDir;
  }
  isDirectory(): boolean {
    return this._isDir;
  }
  isSymbolicLink(): boolean {
    return false;
  }
  isBlockDevice(): boolean {
    return false;
  }
  isCharacterDevice(): boolean {
    return false;
  }
  isFIFO(): boolean {
    return false;
  }
  isSocket(): boolean {
    return false;
  }
}

// Dir class for opendir — async-iterable directory handle
class Dir {
  readonly path: string;
  private _entries: Dirent[] | null = null;
  private _index: number = 0;
  private _closed: boolean = false;

  constructor(dirPath: string) {
    this.path = dirPath;
  }

  private _load(): Dirent[] {
    if (this._entries === null) {
      this._entries = fs.readdirSync(this.path, { withFileTypes: true }) as Dirent[];
    }
    return this._entries;
  }

  readSync(): Dirent | null {
    if (this._closed) throw new Error("Directory handle was closed");
    const entries = this._load();
    if (this._index >= entries.length) return null;
    return entries[this._index++];
  }

  async read(): Promise<Dirent | null> {
    return this.readSync();
  }

  closeSync(): void {
    this._closed = true;
  }

  async close(): Promise<void> {
    this.closeSync();
  }

  async *[Symbol.asyncIterator](): AsyncIterableIterator<Dirent> {
    const entries = this._load();
    for (const entry of entries) {
      if (this._closed) return;
      yield entry;
    }
    this._closed = true;
  }
}

const FILE_HANDLE_READ_CHUNK_BYTES = 64 * 1024;
const FILE_HANDLE_READ_BUFFER_BYTES = 16 * 1024;
const FILE_HANDLE_MAX_READ_BYTES = 2 ** 31 - 1;

function createAbortError(reason?: unknown): Error & { name: string; code?: string; cause?: unknown } {
  const error = new Error("The operation was aborted") as Error & {
    name: string;
    code?: string;
    cause?: unknown;
  };
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  if (reason !== undefined) {
    error.cause = reason;
  }
  return error;
}

function validateAbortSignal(signal: unknown): AbortSignal | undefined {
  if (signal === undefined) {
    return undefined;
  }
  if (
    signal === null ||
    typeof signal !== "object" ||
    typeof (signal as AbortSignal).aborted !== "boolean" ||
    typeof (signal as AbortSignal).addEventListener !== "function" ||
    typeof (signal as AbortSignal).removeEventListener !== "function"
  ) {
    const error = new TypeError(
      'The "signal" argument must be an instance of AbortSignal'
    ) as TypeError & { code?: string };
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }
  return signal as AbortSignal;
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw createAbortError(signal.reason);
  }
}

function waitForNextTick(): Promise<void> {
  return new Promise((resolve) => process.nextTick(resolve));
}

function createInternalAssertionError(message: string): Error & { code: string } {
  const error = new Error(message) as Error & { code: string };
  error.code = "ERR_INTERNAL_ASSERTION";
  return error;
}

function createOutOfRangeError(name: string, range: string, received: unknown): RangeError & { code: string } {
  const error = new RangeError(
    `The value of "${name}" is out of range. It must be ${range}. Received ${String(received)}`
  ) as RangeError & { code: string };
  error.code = "ERR_OUT_OF_RANGE";
  return error;
}

function formatInvalidArgReceived(actual: unknown): string {
  if (actual === null) {
    return "Received null";
  }
  if (actual === undefined) {
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
    const constructorName = (actual as { constructor?: { name?: string } }).constructor?.name;
    if (constructorName) {
      return `Received an instance of ${constructorName}`;
    }
  }
  return `Received type ${typeof actual} (${String(actual)})`;
}

function createInvalidArgTypeError(name: string, expected: string, actual: unknown): TypeError & { code: string } {
  const error = new TypeError(
    `The "${name}" argument must be ${expected}. ${formatInvalidArgReceived(actual)}`
  ) as TypeError & { code: string };
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
}

function createInvalidArgValueError(name: string, message: string): TypeError & { code: string } {
  const error = new TypeError(
    `The argument '${name}' ${message}`
  ) as TypeError & { code: string };
  error.code = "ERR_INVALID_ARG_VALUE";
  return error;
}

function createInvalidEncodingError(encoding: unknown): TypeError & { code: string } {
  const printable =
    typeof encoding === "string"
      ? `'${encoding}'`
      : encoding === undefined
        ? "undefined"
        : encoding === null
          ? "null"
          : String(encoding);
  const error = new TypeError(
    `The argument 'encoding' is invalid encoding. Received ${printable}`
  ) as TypeError & { code: string };
  error.code = "ERR_INVALID_ARG_VALUE";
  return error;
}

function toUint8ArrayChunk(chunk: unknown, encoding?: BufferEncoding): Uint8Array {
  if (typeof chunk === "string") {
    return Buffer.from(chunk, encoding ?? "utf8");
  }
  if (Buffer.isBuffer(chunk)) {
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

async function *iterateWriteChunks(
  data: unknown,
  encoding?: BufferEncoding
): AsyncGenerator<Uint8Array> {
  if (typeof data === "string" || ArrayBuffer.isView(data)) {
    yield toUint8ArrayChunk(data, encoding);
    return;
  }
  if (data && typeof (data as AsyncIterable<unknown>)[Symbol.asyncIterator] === "function") {
    for await (const chunk of data as AsyncIterable<unknown>) {
      yield toUint8ArrayChunk(chunk, encoding);
    }
    return;
  }
  if (data && typeof (data as Iterable<unknown>)[Symbol.iterator] === "function") {
    for (const chunk of data as Iterable<unknown>) {
      yield toUint8ArrayChunk(chunk, encoding);
    }
    return;
  }
  throw createInvalidArgTypeError("data", "a string, Buffer, TypedArray, DataView, or Iterable", data);
}

type FileHandleReadFileOptions = nodeFs.ObjectEncodingOptions & { signal?: AbortSignal | undefined };
type FileHandleWriteFileOptions = nodeFs.ObjectEncodingOptions & { signal?: AbortSignal | undefined };

class FileHandle {
  private _fd: number;
  private _closing = false;
  private _closed = false;
  private _listeners: Map<string | symbol, Array<(...args: unknown[]) => void>> = new Map();

  constructor(fd: number) {
    this._fd = fd;
  }

  private static _assertHandle(handle: unknown): FileHandle {
    if (!(handle instanceof FileHandle)) {
      throw createInternalAssertionError("handle must be an instance of FileHandle");
    }
    return handle;
  }

  private _emitCloseOnce(): void {
    if (this._closed) {
      this._fd = -1;
      this.emit("close");
      return;
    }
    this._closed = true;
    this._fd = -1;
    this.emit("close");
  }

  private _resolvePath(): string | null {
    if (this._fd < 0) {
      return null;
    }
    return _fdGetPath.applySync(undefined, [this._fd]);
  }

  get fd(): number {
    return this._fd;
  }

  get closed(): boolean {
    return this._closed;
  }

  on(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    return this;
  }

  once(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const wrapper = (...args: unknown[]) => {
      this.off(event, wrapper);
      listener(...args);
    };
    (wrapper as { _originalListener?: typeof listener })._originalListener = listener;
    return this.on(event, wrapper);
  }

  off(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const listeners = this._listeners.get(event);
    if (!listeners) {
      return this;
    }
    const index = listeners.findIndex(
      (candidate) =>
        candidate === listener ||
        (candidate as { _originalListener?: typeof listener })._originalListener === listener
    );
    if (index !== -1) {
      listeners.splice(index, 1);
    }
    return this;
  }

  removeListener(event: string | symbol, listener: (...args: unknown[]) => void): this {
    return this.off(event, listener);
  }

  emit(event: string | symbol, ...args: unknown[]): boolean {
    const listeners = this._listeners.get(event);
    if (!listeners || listeners.length === 0) {
      return false;
    }
    for (const listener of listeners.slice()) {
      listener(...args);
    }
    return true;
  }

  async close(): Promise<void> {
    const handle = FileHandle._assertHandle(this);
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

  async stat(): Promise<Stats> {
    const handle = FileHandle._assertHandle(this);
    return fs.fstatSync(handle.fd);
  }

  async sync(): Promise<void> {
    const handle = FileHandle._assertHandle(this);
    fs.fsyncSync(handle.fd);
  }

  async datasync(): Promise<void> {
    return this.sync();
  }

  async truncate(len?: number): Promise<void> {
    const handle = FileHandle._assertHandle(this);
    fs.ftruncateSync(handle.fd, len);
  }

  async chmod(mode: Mode): Promise<void> {
    const handle = FileHandle._assertHandle(this);
    const path = handle._resolvePath();
    if (!path) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chmod");
    }
    fs.chmodSync(path, mode);
  }

  async chown(uid: number, gid: number): Promise<void> {
    const handle = FileHandle._assertHandle(this);
    const path = handle._resolvePath();
    if (!path) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chown");
    }
    fs.chownSync(path, uid, gid);
  }

  async utimes(atime: string | number | Date, mtime: string | number | Date): Promise<void> {
    const handle = FileHandle._assertHandle(this);
    const path = handle._resolvePath();
    if (!path) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "utimes");
    }
    fs.utimesSync(path, atime, mtime);
  }

  async read(
    buffer:
      | NodeJS.ArrayBufferView
      | {
          buffer?: NodeJS.ArrayBufferView | null;
          offset?: number;
          length?: number;
          position?: number | null;
        }
      | null,
    offset?: number,
    length?: number,
    position?: number | null
  ): Promise<{ bytesRead: number; buffer: NodeJS.ArrayBufferView }> {
    const handle = FileHandle._assertHandle(this);
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
      target = Buffer.alloc(FILE_HANDLE_READ_BUFFER_BYTES);
    }
    if (!ArrayBuffer.isView(target)) {
      throw createInvalidArgTypeError("buffer", "an instance of ArrayBufferView", target);
    }
    const normalizedOffset = readOffset ?? 0;
    const normalizedLength = readLength ?? (target.byteLength - normalizedOffset);
    const bytesRead = fs.readSync(
      handle.fd,
      target,
      normalizedOffset,
      normalizedLength,
      readPosition ?? null,
    );
    return { bytesRead, buffer: target };
  }

  async write(
    buffer: string | NodeJS.ArrayBufferView,
    offsetOrPosition?: number,
    lengthOrEncoding?: number | BufferEncoding,
    position?: number
  ): Promise<{ bytesWritten: number; buffer: string | NodeJS.ArrayBufferView }> {
    const handle = FileHandle._assertHandle(this);
    if (typeof buffer === "string") {
      const encoding = typeof lengthOrEncoding === "string" ? lengthOrEncoding : "utf8";
      if (encoding === "hex" && buffer.length % 2 !== 0) {
        throw createInvalidArgValueError("encoding", `is invalid for data of length ${buffer.length}`);
      }
      const bytesWritten = fs.writeSync(handle.fd, Buffer.from(buffer, encoding), 0, undefined, offsetOrPosition ?? null);
      return { bytesWritten, buffer };
    }
    if (!ArrayBuffer.isView(buffer)) {
      throw createInvalidArgTypeError("buffer", "a string, Buffer, TypedArray, or DataView", buffer);
    }
    const offset = offsetOrPosition ?? 0;
    const length = typeof lengthOrEncoding === "number" ? lengthOrEncoding : undefined;
    const bytesWritten = fs.writeSync(handle.fd, buffer, offset, length, position ?? null);
    return { bytesWritten, buffer };
  }

  async readFile(options?: BufferEncoding | FileHandleReadFileOptions | null): Promise<string | Buffer> {
    const handle = FileHandle._assertHandle(this);
    const normalized =
      typeof options === "string" ? { encoding: options } : (options ?? undefined);
    const signal = validateAbortSignal(normalized?.signal);
    const encoding = normalized?.encoding ?? undefined;
    const stats = await handle.stat();
    if (stats.size > FILE_HANDLE_MAX_READ_BYTES) {
      const error = new RangeError("File size is greater than 2 GiB") as RangeError & { code: string };
      error.code = "ERR_FS_FILE_TOO_LARGE";
      throw error;
    }
    await waitForNextTick();
    throwIfAborted(signal);

    const chunks: Buffer[] = [];
    let totalLength = 0;
    while (true) {
      throwIfAborted(signal);
      const chunk = Buffer.alloc(FILE_HANDLE_READ_CHUNK_BYTES);
      const { bytesRead } = await handle.read(chunk, 0, chunk.byteLength, null);
      if (bytesRead === 0) {
        break;
      }
      chunks.push(chunk.subarray(0, bytesRead));
      totalLength += bytesRead;
      if (totalLength > FILE_HANDLE_MAX_READ_BYTES) {
        const error = new RangeError("File size is greater than 2 GiB") as RangeError & { code: string };
        error.code = "ERR_FS_FILE_TOO_LARGE";
        throw error;
      }
      await waitForNextTick();
    }
    const result = Buffer.concat(chunks, totalLength);
    return encoding ? result.toString(encoding) : result;
  }

  async writeFile(
    data: unknown,
    options?: BufferEncoding | FileHandleWriteFileOptions | null
  ): Promise<void> {
    const handle = FileHandle._assertHandle(this);
    const normalized =
      typeof options === "string" ? { encoding: options } : (options ?? undefined);
    const signal = validateAbortSignal(normalized?.signal);
    const encoding = normalized?.encoding ?? undefined;
    await waitForNextTick();
    throwIfAborted(signal);
    for await (const chunk of iterateWriteChunks(data, encoding)) {
      throwIfAborted(signal);
      await handle.write(chunk, 0, chunk.byteLength, undefined);
      await waitForNextTick();
    }
  }

  async appendFile(
    data: unknown,
    options?: BufferEncoding | FileHandleWriteFileOptions | null
  ): Promise<void> {
    return this.writeFile(data, options);
  }

  createReadStream(
    options?: {
      encoding?: BufferEncoding;
      start?: number;
      end?: number;
      highWaterMark?: number;
      signal?: AbortSignal;
    }
  ): ReadStream {
    FileHandle._assertHandle(this);
    return new ReadStream(null, { ...(options ?? {}), fd: this });
  }

  createWriteStream(
    options?: { encoding?: BufferEncoding; flags?: string; mode?: number }
  ): WriteStream {
    FileHandle._assertHandle(this);
    return new WriteStream(null, { ...(options ?? {}), fd: this });
  }
}

type StreamFsMethods = {
  open?: (...args: unknown[]) => unknown;
  close?: (...args: unknown[]) => unknown;
  read?: (...args: unknown[]) => unknown;
  write?: (...args: unknown[]) => unknown;
  writev?: (...args: unknown[]) => unknown;
};

function isArrayBufferView(value: unknown): value is NodeJS.ArrayBufferView {
  return ArrayBuffer.isView(value);
}

function createInvalidPropertyTypeError(propertyPath: string, actual: unknown): TypeError & { code: string } {
  let received: string;
  if (actual === null) {
    received = "Received null";
  } else if (typeof actual === "string") {
    received = `Received type string ('${actual}')`;
  } else {
    received = `Received type ${typeof actual} (${String(actual)})`;
  }
  const error = new TypeError(
    `The "${propertyPath}" property must be of type function. ${received}`
  ) as TypeError & { code: string };
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
}

function validateCallback(callback: unknown, name: string = "cb"): asserts callback is (...args: unknown[]) => void {
  if (typeof callback !== "function") {
    throw createInvalidArgTypeError(name, "of type function", callback);
  }
}

function validateEncodingValue(encoding: unknown): asserts encoding is BufferEncoding {
  if (encoding === undefined || encoding === null) {
    return;
  }
  if (typeof encoding !== "string" || !Buffer.isEncoding(encoding)) {
    throw createInvalidEncodingError(encoding);
  }
}

function validateEncodingOption(options: unknown): void {
  if (typeof options === "string") {
    validateEncodingValue(options);
    return;
  }
  if (options && typeof options === "object" && "encoding" in options) {
    validateEncodingValue((options as { encoding?: unknown }).encoding);
  }
}

function normalizePathLike(path: unknown, name: string = "path"): string {
  if (typeof path === "string") {
    return path;
  }
  if (Buffer.isBuffer(path)) {
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

function tryNormalizeExistsPath(path: unknown): string | null {
  try {
    return normalizePathLike(path);
  } catch {
    return null;
  }
}

function normalizeNumberArgument(
  name: string,
  value: unknown,
  options: { min?: number; max?: number; allowNegativeOne?: boolean } = {},
): number {
  const { min = 0, max = 0x7fffffff, allowNegativeOne = false } = options;
  if (typeof value !== "number") {
    throw createInvalidArgTypeError(name, "of type number", value);
  }
  if (!Number.isFinite(value) || !Number.isInteger(value)) {
    throw createOutOfRangeError(name, "an integer", value);
  }
  if ((allowNegativeOne && value === -1) || (value >= min && value <= max)) {
    return value;
  }
  throw createOutOfRangeError(name, `>= ${min} && <= ${max}`, value);
}

function normalizeModeArgument(mode: unknown, name: string = "mode"): number {
  if (typeof mode === "string") {
    if (!/^[0-7]+$/.test(mode)) {
      throw createInvalidArgValueError(name, "must be a 32-bit unsigned integer or an octal string. Received '" + mode + "'");
    }
    return parseInt(mode, 8);
  }
  return normalizeNumberArgument(name, mode, { min: 0, max: 0xffffffff });
}

function normalizeOpenModeArgument(mode: unknown): number | undefined {
  if (mode === undefined || mode === null) {
    return undefined;
  }
  return normalizeModeArgument(mode);
}

function validateWriteStreamStartOption(options: Record<string, unknown> | undefined): void {
  if (options?.start === undefined) {
    return;
  }
  if (typeof options.start !== "number") {
    throw createInvalidArgTypeError("start", "of type number", options.start);
  }
  if (!Number.isFinite(options.start) || !Number.isInteger(options.start) || options.start < 0) {
    throw createOutOfRangeError("start", ">= 0", options.start);
  }
}

function validateBooleanOption(name: string, value: unknown): boolean | undefined {
  if (value === undefined) {
    return undefined;
  }
  if (typeof value !== "boolean") {
    throw createInvalidArgTypeError(name, "of type boolean", value);
  }
  return value;
}

function validateAbortSignalOption(name: string, signal: unknown): AbortSignal | undefined {
  if (signal === undefined) {
    return undefined;
  }
  if (
    signal === null ||
    typeof signal !== "object" ||
    typeof (signal as AbortSignal).aborted !== "boolean" ||
    typeof (signal as AbortSignal).addEventListener !== "function" ||
    typeof (signal as AbortSignal).removeEventListener !== "function"
  ) {
    const error = new TypeError(
      `The "${name}" property must be an instance of AbortSignal. ${formatInvalidArgReceived(signal)}`
    ) as TypeError & { code?: string };
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }
  return signal as AbortSignal;
}

function createUnsupportedWatcherError(api: "watch" | "watchFile" | "unwatchFile" | "promises.watch"): Error {
  return new Error(`fs.${api} is not supported in sandbox — use polling`);
}

function normalizeWatchOptions(
  options: unknown,
  allowString: boolean,
): {
  persistent?: boolean;
  recursive?: boolean;
  encoding?: BufferEncoding;
  signal?: AbortSignal;
} {
  let normalized: Record<string, unknown>;
  if (options === undefined || options === null) {
    normalized = {};
  } else if (typeof options === "string") {
    if (!allowString) {
      throw createInvalidArgTypeError("options", "of type object", options);
    }
    validateEncodingValue(options);
    normalized = { encoding: options };
  } else if (typeof options === "object") {
    normalized = options as Record<string, unknown>;
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
    persistent: normalized.persistent as boolean | undefined,
    recursive: normalized.recursive as boolean | undefined,
    encoding: normalized.encoding as BufferEncoding | undefined,
    signal,
  };
}

function normalizeWatchArguments(
  path: unknown,
  optionsOrListener?: unknown,
  listener?: unknown,
): {
  persistent?: boolean;
  recursive?: boolean;
  encoding?: BufferEncoding;
  signal?: AbortSignal;
} {
  normalizePathLike(path);

  let options = optionsOrListener;
  let resolvedListener = listener;
  if (typeof optionsOrListener === "function") {
    options = undefined;
    resolvedListener = optionsOrListener;
  }

  if (resolvedListener !== undefined && typeof resolvedListener !== "function") {
    throw createInvalidArgTypeError("listener", "of type function", resolvedListener);
  }

  return normalizeWatchOptions(options, true);
}

function normalizeWatchFileArguments(
  path: unknown,
  optionsOrListener?: unknown,
  listener?: unknown,
): void {
  normalizePathLike(path);

  let options: Record<string, unknown> = {};
  let resolvedListener = listener;

  if (typeof optionsOrListener === "function") {
    resolvedListener = optionsOrListener;
  } else if (optionsOrListener === undefined || optionsOrListener === null) {
    options = {};
  } else if (typeof optionsOrListener === "object") {
    options = optionsOrListener as Record<string, unknown>;
  } else {
    throw createInvalidArgTypeError("listener", "of type function", optionsOrListener);
  }

  if (typeof resolvedListener !== "function") {
    throw createInvalidArgTypeError("listener", "of type function", resolvedListener);
  }

  if (options.interval !== undefined && typeof options.interval !== "number") {
    throw createInvalidArgTypeError("interval", "of type number", options.interval);
  }
}

async function *createUnsupportedPromisesWatchIterator(
  path: unknown,
  options?: unknown,
): AsyncIterableIterator<{ eventType: string; filename: string | Buffer | null }> {
  const normalized = normalizeWatchOptions(options, false);
  normalizePathLike(path);
  throwIfAborted(normalized.signal);
  throw createUnsupportedWatcherError("promises.watch");
}

function isReadWriteOptionsObject(value: unknown): value is Record<string, unknown> {
  return value === null || value === undefined || (typeof value === "object" && !Array.isArray(value));
}

function normalizeOptionalPosition(value: unknown): number | null {
  if (value === undefined || value === null || value === -1) {
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

function normalizeOffsetLength(
  bufferByteLength: number,
  offsetValue: unknown,
  lengthValue: unknown,
): { offset: number; length: number } {
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
  if (length < 0 || length > 0x7fffffff) {
    throw createOutOfRangeError("length", ">= 0 && <= 2147483647", length);
  }
  if (offset + length > bufferByteLength) {
    throw createOutOfRangeError("length", `>= 0 && <= ${bufferByteLength - offset}`, length);
  }

  return { offset, length };
}

function normalizeReadSyncArgs(
  buffer: unknown,
  offsetOrOptions?: number | Record<string, unknown> | null,
  length?: number | null,
  position?: nodeFs.ReadPosition | null,
): {
  buffer: NodeJS.ArrayBufferView;
  offset: number;
  length: number;
  position: number | null;
} {
  if (!isArrayBufferView(buffer)) {
    throw createInvalidArgTypeError("buffer", "an instance of Buffer, TypedArray, or DataView", buffer);
  }

  if (
    length === undefined &&
    position === undefined &&
    isReadWriteOptionsObject(offsetOrOptions)
  ) {
    const options = (offsetOrOptions ?? {}) as Record<string, unknown>;
    const { offset, length } = normalizeOffsetLength(
      buffer.byteLength,
      options.offset,
      options.length,
    );
    return {
      buffer,
      offset,
      length,
      position: normalizeOptionalPosition(options.position),
    };
  }

  const { offset, length: normalizedLength } = normalizeOffsetLength(
    buffer.byteLength,
    offsetOrOptions,
    length,
  );
  return {
    buffer,
    offset,
    length: normalizedLength,
    position: normalizeOptionalPosition(position),
  };
}

function normalizeWriteSyncArgs(
  buffer: unknown,
  offsetOrPosition?: number | Record<string, unknown> | null,
  lengthOrEncoding?: number | BufferEncoding | null,
  position?: number | null,
): {
  buffer: string | NodeJS.ArrayBufferView;
  offset: number;
  length: number;
  position: number | null;
  encoding?: BufferEncoding;
} {
  if (typeof buffer === "string") {
    if (
      lengthOrEncoding === undefined &&
      position === undefined &&
      isReadWriteOptionsObject(offsetOrPosition)
    ) {
      const options = (offsetOrPosition ?? {}) as Record<string, unknown>;
      const encoding = typeof options.encoding === "string" ? (options.encoding as BufferEncoding) : undefined;
      return {
        buffer,
        offset: 0,
        length: Buffer.byteLength(buffer, encoding),
        position: normalizeOptionalPosition(options.position),
        encoding,
      };
    }

    if (
      offsetOrPosition !== undefined &&
      offsetOrPosition !== null &&
      typeof offsetOrPosition !== "number"
    ) {
      throw createInvalidArgTypeError("position", "an integer", offsetOrPosition);
    }

    return {
      buffer,
      offset: 0,
      length: Buffer.byteLength(buffer, typeof lengthOrEncoding === "string" ? lengthOrEncoding : undefined),
      position: normalizeOptionalPosition(offsetOrPosition),
      encoding: typeof lengthOrEncoding === "string" ? lengthOrEncoding : undefined,
    };
  }

  if (!isArrayBufferView(buffer)) {
    throw createInvalidArgTypeError("buffer", "a string, Buffer, TypedArray, or DataView", buffer);
  }

  if (
    lengthOrEncoding === undefined &&
    position === undefined &&
    isReadWriteOptionsObject(offsetOrPosition)
  ) {
    const options = (offsetOrPosition ?? {}) as Record<string, unknown>;
    const { offset, length } = normalizeOffsetLength(
      buffer.byteLength,
      options.offset,
      options.length,
    );
    return {
      buffer,
      offset,
      length,
      position: normalizeOptionalPosition(options.position),
    };
  }

  const { offset, length } = normalizeOffsetLength(
    buffer.byteLength,
    offsetOrPosition,
    typeof lengthOrEncoding === "number" ? lengthOrEncoding : undefined,
  );
  return {
    buffer,
    offset,
    length,
    position: normalizeOptionalPosition(position),
  };
}

function normalizeFdInteger(fd: unknown): number {
  return normalizeNumberArgument("fd", fd);
}

function normalizeIoVectorBuffers(buffers: unknown): ArrayBufferView[] {
  if (!Array.isArray(buffers)) {
    throw createInvalidArgTypeError("buffers", "an ArrayBufferView[]", buffers);
  }
  for (const buffer of buffers) {
    if (!isArrayBufferView(buffer)) {
      throw createInvalidArgTypeError("buffers", "an ArrayBufferView[]", buffers);
    }
  }
  return buffers as ArrayBufferView[];
}

function validateStreamFsOverride(streamFs: unknown, required: Array<keyof StreamFsMethods>): StreamFsMethods | undefined {
  if (streamFs === undefined) {
    return undefined;
  }
  if (streamFs === null || typeof streamFs !== "object") {
    throw createInvalidArgTypeError("options.fs", "an object", streamFs);
  }
  const typed = streamFs as StreamFsMethods;
  for (const key of required) {
    if (typeof typed[key] !== "function") {
      throw createInvalidPropertyTypeError(`options.fs.${String(key)}`, typed[key]);
    }
  }
  return typed;
}

function normalizeStreamFd(fd: unknown): number | FileHandle | undefined {
  if (fd === undefined) {
    return undefined;
  }
  if (fd instanceof FileHandle) {
    return fd;
  }
  return normalizeNumberArgument("fd", fd);
}

function normalizeStreamPath(pathValue: nodeFs.PathLike | null, fd: number | FileHandle | undefined): string | Buffer | null {
  if (pathValue === null) {
    if (fd === undefined) {
      throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", pathValue);
    }
    return null;
  }
  if (typeof pathValue === "string" || Buffer.isBuffer(pathValue)) {
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

function normalizeStreamStartEnd(options: Record<string, unknown> | undefined): {
  start: number | undefined;
  end: number | undefined;
  highWaterMark: number;
  autoClose: boolean;
} {
  const start = options?.start;
  const end = options?.end;

  if (start !== undefined && typeof start !== "number") {
    throw createInvalidArgTypeError("start", "of type number", start);
  }
  if (end !== undefined && typeof end !== "number") {
    throw createInvalidArgTypeError("end", "of type number", end);
  }

  const normalizedStart = start;
  const normalizedEnd = end;

  if (normalizedStart !== undefined && (!Number.isFinite(normalizedStart) || normalizedStart < 0)) {
    throw createOutOfRangeError("start", ">= 0", start);
  }
  if (normalizedEnd !== undefined && (!Number.isFinite(normalizedEnd) || normalizedEnd < 0)) {
    throw createOutOfRangeError("end", ">= 0", end);
  }
  if (
    normalizedStart !== undefined &&
    normalizedEnd !== undefined &&
    normalizedStart > normalizedEnd
  ) {
    throw createOutOfRangeError("start", `<= "end" (here: ${normalizedEnd})`, normalizedStart);
  }

  const highWaterMarkCandidate = options?.highWaterMark ?? options?.bufferSize;
  const highWaterMark =
    typeof highWaterMarkCandidate === "number" && Number.isFinite(highWaterMarkCandidate) && highWaterMarkCandidate > 0
      ? Math.floor(highWaterMarkCandidate)
      : 65536;

  return {
    start: normalizedStart,
    end: normalizedEnd,
    highWaterMark,
    autoClose: options?.autoClose !== false,
  };
}

class ReadStream {
  bytesRead = 0;
  path: string | Buffer | null;
  pending = true;
  readable = true;
  readableAborted = false;
  readableDidRead = false;
  readableEncoding: BufferEncoding | null = null;
  readableEnded = false;
  readableFlowing: boolean | null = null;
  readableHighWaterMark = 65536;
  readableLength = 0;
  readableObjectMode = false;
  destroyed = false;
  closed = false;
  errored: Error | null = null;
  fd: number | null = null;
  autoClose = true;
  start: number | undefined;
  end: number | undefined;

  private _listeners: Map<string | symbol, Array<(...args: unknown[]) => void>> = new Map();
  private _started = false;
  private _reading = false;
  private _readScheduled = false;
  private _opening = false;
  private _remaining: number | null = null;
  private _position: number | null = null;
  private _fileHandle: FileHandle | null = null;
  private _streamFs?: StreamFsMethods;
  private _signal?: AbortSignal;
  private _handleCloseListener?: () => void;

  constructor(
    filePath: string | Buffer | null,
    private _options?: {
      encoding?: BufferEncoding;
      start?: number;
      end?: number;
      highWaterMark?: number;
      bufferSize?: number;
      autoClose?: boolean;
      fd?: number | FileHandle;
      fs?: unknown;
      signal?: AbortSignal;
    }
  ) {
    const fdOption = normalizeStreamFd(_options?.fd);
    const optionsRecord = (_options ?? {}) as Record<string, unknown>;
    const streamState = normalizeStreamStartEnd(optionsRecord);
    this.path = filePath;
    this.start = streamState.start;
    this.end = streamState.end;
    this.autoClose = streamState.autoClose;
    this.readableHighWaterMark = streamState.highWaterMark;
    this.readableEncoding = _options?.encoding ?? null;
    this._position = this.start ?? null;
    this._remaining =
      this.end !== undefined ? this.end - (this.start ?? 0) + 1 : null;
    this._signal = validateAbortSignal(_options?.signal);

    if (fdOption instanceof FileHandle) {
      if (_options?.fs !== undefined) {
        const error = new Error("The FileHandle with fs method is not implemented") as Error & { code?: string };
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

  private _emitOpen(fd: number): void {
    this.fd = fd;
    this.pending = false;
    this.emit("open", fd);
    if (this._started || this.readableFlowing) {
      this._scheduleRead();
    }
  }

  private async _openIfNeeded(): Promise<void> {
    if (this.fd !== null || this._opening || this.destroyed || this.closed) {
      return;
    }
    const pathStr =
      typeof this.path === "string"
        ? this.path
        : this.path instanceof Buffer
          ? this.path.toString()
          : null;
    if (!pathStr) {
      this._handleStreamError(createFsError("EBADF", "EBADF: bad file descriptor", "read"));
      return;
    }

    this._opening = true;
    const opener = (this._streamFs?.open ?? fs.open).bind(this._streamFs ?? fs);
    opener(pathStr, "r", 0o666, (error: Error | null, fd?: number) => {
      this._opening = false;
      if (error || typeof fd !== "number") {
        this._handleStreamError((error as Error) ?? createFsError("EBADF", "EBADF: bad file descriptor", "open"));
        return;
      }
      this._emitOpen(fd);
    });
  }

  private async _closeUnderlying(): Promise<void> {
    if (this._fileHandle) {
      if (!this._fileHandle.closed) {
        await this._fileHandle.close();
      }
      return;
    }
    if (this.fd !== null && this.fd >= 0) {
      const fd = this.fd;
      const closer = (this._streamFs?.close ?? fs.close).bind(this._streamFs ?? fs);
      await new Promise<void>((resolve) => {
        closer(fd, () => resolve());
      });
      this.fd = -1;
    }
  }

  private _scheduleRead(): void {
    if (this._readScheduled || this._reading || this.readableFlowing === false || this.destroyed || this.closed) {
      return;
    }
    this._readScheduled = true;
    queueMicrotask(() => {
      this._readScheduled = false;
      void this._readNextChunk();
    });
  }

  private async _readNextChunk(): Promise<void> {
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

    const nextLength = this._remaining === null
      ? this.readableHighWaterMark
      : Math.min(this.readableHighWaterMark, this._remaining);
    const target = Buffer.alloc(nextLength);

    this._reading = true;
    const onRead = async (error: Error | null, bytesRead: number = 0): Promise<void> => {
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
      this.emit("data", this.readableEncoding ? chunk.toString(this.readableEncoding) : Buffer.from(chunk));

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
        await onRead(error as Error);
      }
      return;
    }

    const reader = (this._streamFs?.read ?? fs.read).bind(this._streamFs ?? fs);
    reader(this.fd, target, 0, nextLength, this._position, (error: Error | null, bytesRead?: number) => {
      void onRead(error, bytesRead ?? 0);
    });
  }

  private async _finishReadable(): Promise<void> {
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

  private _handleStreamError(error: Error): void {
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

  private async _abort(reason?: unknown): Promise<void> {
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

  private async _readAllContent(): Promise<Buffer> {
    const chunks: Buffer[] = [];
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
      const nextLength = this._remaining === null
        ? FILE_HANDLE_READ_CHUNK_BYTES
        : Math.min(FILE_HANDLE_READ_CHUNK_BYTES, this._remaining);
      const target = Buffer.alloc(nextLength);
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
    return Buffer.concat(chunks, totalLength);
  }

  on(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    if (event === "data") {
      this._started = true;
      this.readableFlowing = true;
      this._scheduleRead();
    }
    return this;
  }

  once(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const wrapper = (...args: unknown[]): void => {
      this.off(event, wrapper);
      listener(...args);
    };
    (wrapper as { _originalListener?: typeof listener })._originalListener = listener;
    return this.on(event, wrapper);
  }

  off(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const listeners = this._listeners.get(event);
    if (!listeners) {
      return this;
    }
    const index = listeners.findIndex(
      (fn) => fn === listener || (fn as { _originalListener?: typeof listener })._originalListener === listener,
    );
    if (index >= 0) {
      listeners.splice(index, 1);
    }
    return this;
  }

  removeListener(event: string | symbol, listener: (...args: unknown[]) => void): this {
    return this.off(event, listener);
  }

  removeAllListeners(event?: string | symbol): this {
    if (event === undefined) {
      this._listeners.clear();
    } else {
      this._listeners.delete(event);
    }
    return this;
  }

  emit(event: string | symbol, ...args: unknown[]): boolean {
    const listeners = this._listeners.get(event);
    if (!listeners?.length) {
      return false;
    }
    listeners.slice().forEach((listener) => listener(...args));
    return true;
  }

  read(): Buffer | string | null {
    return null;
  }

  pipe<T extends NodeJS.WritableStream>(destination: T, _options?: { end?: boolean }): T {
    this.on("data", (chunk) => {
      destination.write(chunk as string);
    });
    this.on("end", () => {
      destination.end?.();
    });
    this.resume();
    return destination;
  }

  unpipe(_destination?: NodeJS.WritableStream): this {
    return this;
  }

  pause(): this {
    this.readableFlowing = false;
    return this;
  }

  resume(): this {
    this._started = true;
    this.readableFlowing = true;
    this._scheduleRead();
    return this;
  }

  setEncoding(encoding: BufferEncoding): this {
    this.readableEncoding = encoding;
    return this;
  }

  destroy(error?: Error): this {
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

  close(callback?: (err?: Error | null) => void): void {
    this.destroy();
    if (callback) {
      queueMicrotask(() => callback(null));
    }
  }

  async *[Symbol.asyncIterator](): AsyncIterator<Buffer | string> {
    const content = await this._readAllContent();
    yield this.readableEncoding ? content.toString(this.readableEncoding) : content;
  }
}

const MAX_WRITE_STREAM_BYTES = 16 * 1024 * 1024;

class WriteStream {
  bytesWritten = 0;
  path: string | Buffer | null;
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
  errored: Error | null = null;
  writableNeedDrain = false;
  fd: number | null = null;
  autoClose = true;

  private _chunks: Uint8Array[] = [];
  private _listeners: Map<string | symbol, Array<(...args: unknown[]) => void>> = new Map();
  private _fileHandle: FileHandle | null = null;
  private _streamFs?: StreamFsMethods;

  constructor(
    filePath: string | Buffer | null,
    private _options?: { encoding?: BufferEncoding; flags?: string; mode?: number; fd?: number | FileHandle; fs?: unknown; autoClose?: boolean }
  ) {
    const fdOption = normalizeStreamFd(_options?.fd);
    this.path = filePath;
    this.autoClose = _options?.autoClose !== false;
    this._streamFs = validateStreamFsOverride(_options?.fs, ["open", "close", "write"]);
    if (_options?.fs !== undefined) {
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

    const pathStr =
      typeof this.path === "string"
        ? this.path
        : this.path instanceof Buffer
          ? this.path.toString()
          : null;
    if (!pathStr) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
    }
    this.fd = fs.openSync(pathStr, _options?.flags ?? "w", _options?.mode);
    queueMicrotask(() => {
      if (this.fd !== null && this.fd >= 0) {
        this.emit("open", this.fd);
      }
    });
  }

  private async _closeUnderlying(): Promise<void> {
    if (this._fileHandle) {
      if (!this._fileHandle.closed) {
        await this._fileHandle.close();
      }
      return;
    }
    if (this.fd !== null && this.fd >= 0) {
      const fd = this.fd;
      const closer = (this._streamFs?.close ?? fs.close).bind(this._streamFs ?? fs);
      await new Promise<void>((resolve) => {
        closer(fd, () => resolve());
      });
      this.fd = -1;
    }
  }

  close(callback?: (err?: NodeJS.ErrnoException | null) => void): void {
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

  write(
    chunk: unknown,
    encodingOrCallback?: BufferEncoding | ((error: Error | null | undefined) => void),
    callback?: (error: Error | null | undefined) => void
  ): boolean {
    if (this.writableEnded || this.destroyed) {
      const error = new Error("write after end");
      const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      queueMicrotask(() => cb?.(error));
      return false;
    }

    let data: Uint8Array;
    if (typeof chunk === "string") {
      data = Buffer.from(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : "utf8");
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
      const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      queueMicrotask(() => {
        cb?.(error);
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

  end(chunkOrCb?: unknown, encodingOrCallback?: BufferEncoding | (() => void), callback?: () => void): this {
    if (this.writableEnded) {
      return this;
    }

    let cb: (() => void) | undefined;
    if (typeof chunkOrCb === "function") {
      cb = chunkOrCb as () => void;
    } else if (typeof encodingOrCallback === "function") {
      cb = encodingOrCallback;
      if (chunkOrCb !== undefined && chunkOrCb !== null) {
        this.write(chunkOrCb);
      }
    } else {
      cb = callback;
      if (chunkOrCb !== undefined && chunkOrCb !== null) {
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
              await this._fileHandle.write(chunk, 0, chunk.byteLength, undefined);
            }
            if (this.autoClose && !this._fileHandle.closed) {
              await this._fileHandle.close();
            }
          } else if (this.fd !== null && this.fd >= 0) {
            for (const chunk of this._chunks) {
              fs.writeSync(this.fd, chunk, 0, chunk.byteLength, null);
            }
            if (this.autoClose) {
              await this._closeUnderlying();
            }
          } else {
            const pathStr =
              typeof this.path === "string"
                ? this.path
                : this.path instanceof Buffer
                  ? this.path.toString()
                  : null;
            if (!pathStr) {
              throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
            }
            fs.writeFileSync(pathStr, Buffer.concat(this._chunks.map((chunk) => Buffer.from(chunk))));
          }
          this.emit("finish");
          if (this.autoClose && !this.closed) {
            this.closed = true;
            this.emit("close");
          }
          cb?.();
        } catch (error) {
          this.errored = error as Error;
          this.emit("error", error);
        }
      })();
    });

    return this;
  }

  setDefaultEncoding(_encoding: BufferEncoding): this {
    return this;
  }

  cork(): void {
    this.writableCorked++;
  }

  uncork(): void {
    if (this.writableCorked > 0) {
      this.writableCorked--;
    }
  }

  destroy(error?: Error): this {
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

  addListener(event: string | symbol, listener: (...args: unknown[]) => void): this {
    return this.on(event, listener);
  }

  on(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    return this;
  }

  once(event: string | symbol, listener: (...args: unknown[]) => void): this {
    const wrapper = (...args: unknown[]): void => {
      this.removeListener(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }

  removeListener(event: string | symbol, listener: (...args: unknown[]) => void): this {
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

  off(event: string | symbol, listener: (...args: unknown[]) => void): this {
    return this.removeListener(event, listener);
  }

  removeAllListeners(event?: string | symbol): this {
    if (event === undefined) {
      this._listeners.clear();
    } else {
      this._listeners.delete(event);
    }
    return this;
  }

  emit(event: string | symbol, ...args: unknown[]): boolean {
    const listeners = this._listeners.get(event);
    if (!listeners?.length) {
      return false;
    }
    listeners.slice().forEach((listener) => listener(...args));
    return true;
  }

  pipe<T extends NodeJS.WritableStream>(destination: T, _options?: { end?: boolean }): T {
    return destination;
  }

  unpipe(_destination?: NodeJS.WritableStream): this {
    return this;
  }

  [Symbol.asyncDispose](): Promise<void> {
    return Promise.resolve();
  }
}

const ReadStreamClass = ReadStream;
const WriteStreamClass = WriteStream;

const ReadStreamFactory = function ReadStream(
  path: string | Buffer | null,
  options?: {
    encoding?: BufferEncoding;
    start?: number;
    end?: number;
    highWaterMark?: number;
    bufferSize?: number;
    autoClose?: boolean;
    fd?: number | FileHandle;
    fs?: unknown;
    signal?: AbortSignal;
  },
): ReadStream {
  validateEncodingOption(options);
  return new ReadStreamClass(path, options);
};
ReadStreamFactory.prototype = ReadStream.prototype;

const WriteStreamFactory = function WriteStream(
  path: string | Buffer | null,
  options?: {
    encoding?: BufferEncoding;
    flags?: string;
    mode?: number;
    fd?: number | FileHandle;
    fs?: unknown;
    autoClose?: boolean;
  },
): WriteStream {
  validateEncodingOption(options);
  validateWriteStreamStartOption((options ?? {}) as Record<string, unknown>);
  return new WriteStreamClass(path, options);
};
WriteStreamFactory.prototype = WriteStream.prototype;

// Parse flags string to number
function parseFlags(flags: OpenMode): number {
  if (typeof flags === "number") return flags;
  const flagMap: Record<string, number> = {
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
    "xa+": O_RDWR | O_APPEND | O_CREAT | O_EXCL,
  };
  if (flags in flagMap) return flagMap[flags];
  throw new Error("Unknown file flag: " + flags);
}

// Helper to create fs errors
function createFsError(
  code: string,
  message: string,
  syscall: string,
  path?: string
): Error & { code: string; errno: number; syscall: string; path?: string } {
  const err = new Error(message) as Error & {
    code: string;
    errno: number;
    syscall: string;
    path?: string;
  };
  err.code = code;
  err.errno = code === "ENOENT" ? -2 : code === "EACCES" ? -13 : code === "EBADF" ? -9 : code === "EMFILE" ? -24 : -1;
  err.syscall = syscall;
  if (path) err.path = path;
  return err;
}

/** Wrap a bridge call with ENOENT/EACCES error re-creation. */
function bridgeCall<T>(fn: () => T, syscall: string, path?: string): T {
  try {
    return fn();
  } catch (err) {
    const msg = (err as Error).message || String(err);
    if (msg.includes("ENOENT") || msg.includes("no such file or directory") || msg.includes("not found")) {
      throw createFsError("ENOENT", `ENOENT: no such file or directory, ${syscall} '${path}'`, syscall, path);
    }
    if (msg.includes("EACCES") || msg.includes("permission denied")) {
      throw createFsError("EACCES", `EACCES: permission denied, ${syscall} '${path}'`, syscall, path);
    }
    if (msg.includes("EEXIST") || msg.includes("file already exists")) {
      throw createFsError("EEXIST", `EEXIST: file already exists, ${syscall} '${path}'`, syscall, path);
    }
    if (msg.includes("EINVAL") || msg.includes("invalid argument")) {
      throw createFsError("EINVAL", `EINVAL: invalid argument, ${syscall} '${path}'`, syscall, path);
    }
    throw err;
  }
}

// Glob pattern matching helper — converts glob to regex and walks VFS recursively
function _globToRegex(pattern: string): RegExp {
  // Determine base directory vs glob portion
  let regexStr = "";
  let i = 0;
  while (i < pattern.length) {
    const ch = pattern[i];
    if (ch === "*" && pattern[i + 1] === "*") {
      // ** matches any depth of directories
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
        regexStr += "(?:" + alternatives.map(a => a.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\\\*/g, "[^/]*")).join("|") + ")";
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

function _globGetBase(pattern: string): string {
  // Find the longest directory prefix that has no glob characters
  const parts = pattern.split("/");
  const baseParts: string[] = [];
  for (const part of parts) {
    if (/[*?{}\[\]]/.test(part)) break;
    baseParts.push(part);
  }
  return baseParts.join("/") || "/";
}

// Recursively walk VFS directory and collect matching paths
// We use a reference to `fs` via late-binding in the fs object method
const MAX_GLOB_DEPTH = 100; // Prevent stack overflow on deeply nested trees

function _globCollect(pattern: string, results: string[]): void {
  const regex = _globToRegex(pattern);
  const base = _globGetBase(pattern);

  const walk = (dir: string, depth: number): void => {
    if (depth > MAX_GLOB_DEPTH) return;
    let entries: string[];
    try {
      entries = _globReadDir(dir);
    } catch {
      return; // Directory doesn't exist or not readable
    }
    for (const entry of entries) {
      const fullPath = dir === "/" ? "/" + entry : dir + "/" + entry;
      // Check if this path matches the pattern
      if (regex.test(fullPath)) {
        results.push(fullPath);
      }
      // Recurse into directories if pattern has ** or more segments
      try {
        const stat = _globStat(fullPath);
        if (stat.isDirectory()) {
          walk(fullPath, depth + 1);
        }
      } catch {
        // Not a directory or stat failed — skip
      }
    }
  };

  // Start walking from the base directory
  try {
    // Check if base itself matches (edge case)
    if (regex.test(base)) {
      const stat = _globStat(base);
      if (!stat.isDirectory()) {
        results.push(base);
        return;
      }
    }
    walk(base, 0);
  } catch {
    // Base doesn't exist — no matches
  }
}

// Late-bound references — these get assigned after fs is defined
let _globReadDir: (dir: string) => string[];
let _globStat: (path: string) => Stats;

// Type definitions for the fs module - use Node.js types
type PathLike = nodeFs.PathLike;
type PathOrFileDescriptor = nodeFs.PathOrFileDescriptor;
type OpenMode = nodeFs.OpenMode;
type Mode = nodeFs.Mode;
type ReadFileOptions = Parameters<typeof nodeFs.readFileSync>[1];
type WriteFileOptions = nodeFs.WriteFileOptions;
type MakeDirectoryOptions = nodeFs.MakeDirectoryOptions;
type RmDirOptions = nodeFs.RmDirOptions;
type ReaddirOptions = nodeFs.ObjectEncodingOptions & { withFileTypes?: boolean; recursive?: boolean };
type MkdirOptions = MakeDirectoryOptions;
type OpenFlags = nodeFs.OpenMode;
type NodeCallback<T> = (err: NodeJS.ErrnoException | null, result?: T) => void;

// Helper to convert PathLike to string
function toPathString(path: PathLike): string {
  return normalizePathLike(path);
}

// Note: Path normalization is handled by VirtualFileSystem, not here.
// The VFS expects /data/* paths for Directory access, so we pass paths through unchanged.

// The fs module implementation
const fs = {
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
    UV_FS_O_FILEMAP: 536870912,
  },

  Stats,
  Dirent,
  Dir,

  // Sync methods

  readFileSync(path: PathOrFileDescriptor, options?: ReadFileOptions): string | Buffer {
    validateEncodingOption(options);
    const rawPath = typeof path === "number"
      ? _fdGetPath.applySync(undefined, [normalizeFdInteger(path)])
      : normalizePathLike(path);
    if (!rawPath) throw createFsError("EBADF", "EBADF: bad file descriptor", "read");
    const pathStr = rawPath;
    const encoding =
      typeof options === "string" ? options : (options as { encoding?: BufferEncoding | null })?.encoding;

    try {
      if (encoding) {
        // Text mode - use text read
        const content = _fs.readFile.applySyncPromise(undefined, [pathStr]);
        return content;
      } else {
        // Binary mode - use binary read with base64 encoding
        const base64Content = _fs.readFileBinary.applySyncPromise(undefined, [pathStr]);
        return Buffer.from(base64Content, "base64");
      }
    } catch (err) {
      const errMsg = (err as Error).message || String(err);
      // Convert various "not found" errors to proper ENOENT
      if (
        errMsg.includes("entry not found") ||
        errMsg.includes("not found") ||
        errMsg.includes("ENOENT") ||
        errMsg.includes("no such file or directory")
      ) {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, open '${rawPath}'`,
          "open",
          rawPath
        );
      }
      // Convert permission errors to proper EACCES
      if (errMsg.includes("EACCES") || errMsg.includes("permission denied")) {
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

  writeFileSync(
    file: PathOrFileDescriptor,
    data: string | NodeJS.ArrayBufferView,
    _options?: WriteFileOptions
  ): void {
    validateEncodingOption(_options);
    const rawPath = typeof file === "number"
      ? _fdGetPath.applySync(undefined, [normalizeFdInteger(file)])
      : normalizePathLike(file);
    if (!rawPath) throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
    const pathStr = rawPath;

    if (typeof data === "string") {
      // Text mode - use text write
      // Return the result so async callers (fs.promises) can await it.
      return _fs.writeFile.applySyncPromise(undefined, [pathStr, data]);
    } else if (ArrayBuffer.isView(data)) {
      // Binary mode - convert to base64 and use binary write
      const uint8 = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      const base64 = Buffer.from(uint8).toString("base64");
      return _fs.writeFileBinary.applySyncPromise(undefined, [pathStr, base64]);
    } else {
      // Fallback to text mode
      return _fs.writeFile.applySyncPromise(undefined, [pathStr, String(data)]);
    }
  },

  appendFileSync(
    path: PathOrFileDescriptor,
    data: string | Uint8Array,
    options?: WriteFileOptions
  ): void {
    validateEncodingOption(options);
    const existing = fs.existsSync(path as PathLike)
      ? (fs.readFileSync(path, "utf8") as string)
      : "";
    const content = typeof data === "string" ? data : String(data);
    fs.writeFileSync(path, existing + content, options);
  },

  readdirSync(path: PathLike, options?: nodeFs.ObjectEncodingOptions & { withFileTypes?: boolean; recursive?: boolean }): string[] | Dirent[] {
    validateEncodingOption(options);
    const rawPath = normalizePathLike(path);
    const pathStr = rawPath;
    let entriesJson: string;
    try {
      entriesJson = _fs.readDir.applySyncPromise(undefined, [pathStr]);
    } catch (err) {
      // Convert "entry not found" and similar errors to proper ENOENT
      const errMsg = (err as Error).message || String(err);
      if (errMsg.includes("entry not found") || errMsg.includes("not found")) {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, scandir '${rawPath}'`,
          "scandir",
          rawPath
        );
      }
      throw err;
    }
    const entries = JSON.parse(entriesJson) as Array<{
      name: string;
      isDirectory: boolean;
    }>;
    if (options?.withFileTypes) {
      return entries.map((e) => new Dirent(e.name, e.isDirectory, rawPath));
    }
    return entries.map((e) => e.name);
  },

  mkdirSync(path: PathLike, options?: MakeDirectoryOptions | Mode): string | undefined {
    const rawPath = normalizePathLike(path);
    const pathStr = rawPath;
    const recursive = typeof options === "object" ? options?.recursive ?? false : false;
    _fs.mkdir.applySyncPromise(undefined, [pathStr, recursive]);
    return recursive ? rawPath : undefined;
  },

  rmdirSync(path: PathLike, _options?: RmDirOptions): void {
    const pathStr = normalizePathLike(path);
    _fs.rmdir.applySyncPromise(undefined, [pathStr]);
  },

  rmSync(path: PathLike, options?: { force?: boolean; recursive?: boolean }): void {
    const pathStr = toPathString(path);
    const opts = options || {};
    try {
      const stats = fs.statSync(pathStr);
      if (stats.isDirectory()) {
        if (opts.recursive) {
          // Recursively remove directory contents
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
      if (opts.force && (e as NodeJS.ErrnoException).code === "ENOENT") {
        return; // Ignore ENOENT when force is true
      }
      throw e;
    }
  },

  existsSync(path: PathLike): boolean {
    const pathStr = tryNormalizeExistsPath(path);
    if (!pathStr) {
      return false;
    }
    return _fs.exists.applySyncPromise(undefined, [pathStr]);
  },

  statSync(path: PathLike, _options?: nodeFs.StatSyncOptions): Stats {
    const rawPath = normalizePathLike(path);
    const pathStr = rawPath;
    let statJson: string;
    try {
      statJson = _fs.stat.applySyncPromise(undefined, [pathStr]);
    } catch (err) {
      // Convert various "not found" errors to proper ENOENT
      const errMsg = (err as Error).message || String(err);
      if (
        errMsg.includes("entry not found") ||
        errMsg.includes("not found") ||
        errMsg.includes("ENOENT") ||
        errMsg.includes("no such file or directory")
      ) {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, stat '${rawPath}'`,
          "stat",
          rawPath
        );
      }
      throw err;
    }
    const stat = JSON.parse(statJson) as {
      mode: number;
      size: number;
      atimeMs?: number;
      mtimeMs?: number;
      ctimeMs?: number;
      birthtimeMs?: number;
    };
    return new Stats(stat);
  },

  lstatSync(path: PathLike, _options?: nodeFs.StatSyncOptions): Stats {
    const pathStr = normalizePathLike(path);
    const statJson = bridgeCall(() => _fs.lstat.applySyncPromise(undefined, [pathStr]), "lstat", pathStr);
    const stat = JSON.parse(statJson) as {
      mode: number;
      size: number;
      isDirectory: boolean;
      isSymbolicLink?: boolean;
      atimeMs?: number;
      mtimeMs?: number;
      ctimeMs?: number;
      birthtimeMs?: number;
    };
    return new Stats(stat);
  },

  unlinkSync(path: PathLike): void {
    const pathStr = normalizePathLike(path);
    _fs.unlink.applySyncPromise(undefined, [pathStr]);
  },

  renameSync(oldPath: PathLike, newPath: PathLike): void {
    const oldPathStr = normalizePathLike(oldPath, "oldPath");
    const newPathStr = normalizePathLike(newPath, "newPath");
    _fs.rename.applySyncPromise(undefined, [oldPathStr, newPathStr]);
  },

  copyFileSync(src: PathLike, dest: PathLike, _mode?: number): void {
    // readFileSync and writeFileSync already normalize paths
    const content = fs.readFileSync(src);
    fs.writeFileSync(dest, content as Buffer);
  },

  // Recursive copy
  cpSync(src: PathLike, dest: PathLike, options?: { recursive?: boolean; force?: boolean; errorOnExist?: boolean }): void {
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
      // Create destination directory
      try {
        fs.mkdirSync(destPath, { recursive: true });
      } catch {
        // May already exist
      }
      // Copy contents recursively
      const entries = fs.readdirSync(srcPath) as string[];
      for (const entry of entries) {
        const srcEntry = srcPath.endsWith("/") ? srcPath + entry : srcPath + "/" + entry;
        const destEntry = destPath.endsWith("/") ? destPath + entry : destPath + "/" + entry;
        fs.cpSync(srcEntry, destEntry, opts);
      }
    } else {
      // File copy
      if (opts.errorOnExist && fs.existsSync(destPath)) {
        throw createFsError(
          "EEXIST",
          `EEXIST: file already exists, cp '${srcPath}' -> '${destPath}'`,
          "cp",
          destPath
        );
      }
      if (!opts.force && opts.force !== undefined && fs.existsSync(destPath)) {
        return; // Skip without error when force is false
      }
      fs.copyFileSync(srcPath, destPath);
    }
  },

  // Temp directory creation
  mkdtempSync(prefix: string, _options?: nodeFs.EncodingOption): string {
    validateEncodingOption(_options);
    const suffix = Math.random().toString(36).slice(2, 8);
    const dirPath = prefix + suffix;
    fs.mkdirSync(dirPath, { recursive: true });
    return dirPath;
  },

  // Directory handle (sync)
  opendirSync(path: PathLike, _options?: nodeFs.OpenDirOptions): Dir {
    const pathStr = normalizePathLike(path);
    // Verify directory exists
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

  openSync(path: PathLike, flags?: OpenMode, _mode?: Mode | null): number {
    const pathStr = normalizePathLike(path);
    const numFlags = parseFlags(flags ?? "r");
    const modeNum = normalizeOpenModeArgument(_mode);
    try {
      return _fdOpen.applySyncPromise(undefined, [pathStr, numFlags, modeNum]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("ENOENT")) throw createFsError("ENOENT", msg, "open", pathStr);
      if (msg.includes("EMFILE")) throw createFsError("EMFILE", msg, "open", pathStr);
      throw e;
    }
  },

  closeSync(fd: number): void {
    normalizeFdInteger(fd);
    try {
      _fdClose.applySyncPromise(undefined, [fd]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, close", "close");
      throw e;
    }
  },

  readSync(
    fd: number,
    buffer: NodeJS.ArrayBufferView,
    offset?: number | Record<string, unknown> | null,
    length?: number | null,
    position?: nodeFs.ReadPosition | null
  ): number {
    const normalized = normalizeReadSyncArgs(buffer, offset, length, position);

    let base64: string;
    try {
      base64 = _fdRead.applySyncPromise(undefined, [fd, normalized.length, normalized.position ?? null]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", msg, "read");
      throw e;
    }

    const bytes = Buffer.from(base64, "base64");
    const targetBuffer = new Uint8Array(
      normalized.buffer.buffer,
      normalized.buffer.byteOffset,
      normalized.buffer.byteLength,
    );
    for (let i = 0; i < bytes.length && i < normalized.length; i++) {
      targetBuffer[normalized.offset + i] = bytes[i];
    }
    return bytes.length;
  },

  writeSync(
    fd: number,
    buffer: string | NodeJS.ArrayBufferView,
    offsetOrPosition?: number | Record<string, unknown> | null,
    lengthOrEncoding?: number | BufferEncoding | null,
    position?: number | null
  ): number {
    const normalized = normalizeWriteSyncArgs(buffer, offsetOrPosition, lengthOrEncoding, position);
    let dataBytes: Uint8Array;
    if (typeof normalized.buffer === "string") {
      dataBytes = Buffer.from(normalized.buffer, normalized.encoding);
    } else {
      dataBytes = new Uint8Array(
        normalized.buffer.buffer,
        normalized.buffer.byteOffset + normalized.offset,
        normalized.length,
      );
    }

    const base64 = Buffer.from(dataBytes).toString("base64");
    const pos = normalized.position ?? null;

    try {
      return _fdWrite.applySyncPromise(undefined, [fd, base64, pos]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", msg, "write");
      throw e;
    }
  },

  fstatSync(fd: number): Stats {
    normalizeFdInteger(fd);
    let raw: string;
    try {
      raw = _fdFstat.applySyncPromise(undefined, [fd]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fstat", "fstat");
      throw e;
    }
    return new Stats(JSON.parse(raw));
  },

  ftruncateSync(fd: number, len?: number): void {
    normalizeFdInteger(fd);
    try {
      _fdFtruncate.applySyncPromise(undefined, [fd, len]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, ftruncate", "ftruncate");
      throw e;
    }
  },

  // fsync / fdatasync — no-op for in-memory VFS (validates FD exists)
  fsyncSync(fd: number): void {
    normalizeFdInteger(fd);
    try {
      _fdFsync.applySyncPromise(undefined, [fd]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fsync", "fsync");
      throw e;
    }
  },

  fdatasyncSync(fd: number): void {
    normalizeFdInteger(fd);
    try {
      _fdFsync.applySyncPromise(undefined, [fd]);
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fdatasync", "fdatasync");
      throw e;
    }
  },

  // readv — scatter-read into multiple buffers (delegates to readSync)
  readvSync(fd: number, buffers: ArrayBufferView[], position?: number | null): number {
    const normalizedFd = normalizeFdInteger(fd);
    const normalizedBuffers = normalizeIoVectorBuffers(buffers);
    let totalBytesRead = 0;
    const normalizedPosition = normalizeOptionalPosition(position);
    let nextPosition = normalizedPosition;
    for (const buffer of normalizedBuffers) {
      const target = buffer instanceof Uint8Array
        ? buffer
        : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
      const bytesRead = fs.readSync(normalizedFd, target, 0, target.byteLength, nextPosition);
      totalBytesRead += bytesRead;
      if (nextPosition !== null) {
        nextPosition += bytesRead;
      }
      // EOF — stop filling further buffers
      if (bytesRead < target.byteLength) break;
    }
    return totalBytesRead;
  },

  // statfs — return synthetic filesystem stats for the in-memory VFS
  statfsSync(path: PathLike, _options?: nodeFs.StatFsOptions): nodeFs.StatsFs {
    const pathStr = normalizePathLike(path);
    // Verify path exists
    if (!fs.existsSync(pathStr)) {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, statfs '${pathStr}'`,
        "statfs",
        pathStr
      );
    }
    // Return synthetic stats — in-memory VFS has no real block device
    return {
      type: 0x01021997, // TMPFS_MAGIC
      bsize: 4096,
      blocks: 262144,    // 1GB virtual capacity
      bfree: 262144,
      bavail: 262144,
      files: 1000000,
      ffree: 999999,
    } as unknown as nodeFs.StatsFs;
  },

  // glob — pattern matching over VFS files
  globSync(pattern: string | string[], _options?: nodeFs.GlobOptionsWithFileTypes): string[] {
    const patterns = Array.isArray(pattern) ? pattern : [pattern];
    const results: string[] = [];
    for (const pat of patterns) {
      _globCollect(pat, results);
    }
    return [...new Set(results)].sort();
  },

  // Metadata and link sync methods — delegate to VFS via host refs
  chmodSync(path: PathLike, mode: Mode): void {
    const pathStr = normalizePathLike(path);
    const modeNum = normalizeModeArgument(mode);
    bridgeCall(() => _fs.chmod.applySyncPromise(undefined, [pathStr, modeNum]), "chmod", pathStr);
  },

  chownSync(path: PathLike, uid: number, gid: number): void {
    const pathStr = normalizePathLike(path);
    const normalizedUid = normalizeNumberArgument("uid", uid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
    const normalizedGid = normalizeNumberArgument("gid", gid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
    bridgeCall(() => _fs.chown.applySyncPromise(undefined, [pathStr, normalizedUid, normalizedGid]), "chown", pathStr);
  },

  fchmodSync(fd: number, mode: Mode): void {
    const normalizedFd = normalizeFdInteger(fd);
    const pathStr = _fdGetPath.applySync(undefined, [normalizedFd]);
    if (!pathStr) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chmod");
    }
    fs.chmodSync(pathStr, normalizeModeArgument(mode));
  },

  fchownSync(fd: number, uid: number, gid: number): void {
    const normalizedFd = normalizeFdInteger(fd);
    const pathStr = _fdGetPath.applySync(undefined, [normalizedFd]);
    if (!pathStr) {
      throw createFsError("EBADF", "EBADF: bad file descriptor", "chown");
    }
    fs.chownSync(pathStr, uid, gid);
  },

  lchownSync(path: PathLike, uid: number, gid: number): void {
    const pathStr = normalizePathLike(path);
    const normalizedUid = normalizeNumberArgument("uid", uid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
    const normalizedGid = normalizeNumberArgument("gid", gid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
    bridgeCall(() => _fs.chown.applySyncPromise(undefined, [pathStr, normalizedUid, normalizedGid]), "chown", pathStr);
  },

  linkSync(existingPath: PathLike, newPath: PathLike): void {
    const existingStr = normalizePathLike(existingPath, "existingPath");
    const newStr = normalizePathLike(newPath, "newPath");
    bridgeCall(() => _fs.link.applySyncPromise(undefined, [existingStr, newStr]), "link", newStr);
  },

  symlinkSync(target: PathLike, path: PathLike, _type?: string | null): void {
    const targetStr = normalizePathLike(target, "target");
    const pathStr = normalizePathLike(path);
    bridgeCall(() => _fs.symlink.applySyncPromise(undefined, [targetStr, pathStr]), "symlink", pathStr);
  },

  readlinkSync(path: PathLike, _options?: nodeFs.EncodingOption): string {
    validateEncodingOption(_options);
    const pathStr = normalizePathLike(path);
    return bridgeCall(() => _fs.readlink.applySyncPromise(undefined, [pathStr]), "readlink", pathStr);
  },

  truncateSync(path: PathLike, len?: number | null): void {
    const pathStr = normalizePathLike(path);
    bridgeCall(() => _fs.truncate.applySyncPromise(undefined, [pathStr, len ?? 0]), "truncate", pathStr);
  },

  utimesSync(path: PathLike, atime: string | number | Date, mtime: string | number | Date): void {
    const pathStr = normalizePathLike(path);
    const atimeNum = typeof atime === "number" ? atime : new Date(atime).getTime() / 1000;
    const mtimeNum = typeof mtime === "number" ? mtime : new Date(mtime).getTime() / 1000;
    bridgeCall(() => _fs.utimes.applySyncPromise(undefined, [pathStr, atimeNum, mtimeNum]), "utimes", pathStr);
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

  readFile(
    path: string,
    options?: ReadFileOptions | NodeCallback<string | Uint8Array>,
    callback?: NodeCallback<string | Uint8Array>
  ): Promise<string | Uint8Array> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(options);
      try {
        callback(null, fs.readFileSync(path, options));
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.readFileSync(path, options as ReadFileOptions));
    }
  },

  writeFile(
    path: string,
    data: string | Uint8Array,
    options?: WriteFileOptions | NodeCallback<void>,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(options);
      try {
        fs.writeFileSync(path, data, options);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(
        fs.writeFileSync(path, data, options as WriteFileOptions)
      );
    }
  },

  appendFile(
    path: string,
    data: string | Uint8Array,
    options?: WriteFileOptions | NodeCallback<void>,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(options);
      try {
        fs.appendFileSync(path, data, options);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(
        fs.appendFileSync(path, data, options as WriteFileOptions)
      );
    }
  },

  readdir(
    path: string,
    options?: ReaddirOptions | NodeCallback<string[] | Dirent[]>,
    callback?: NodeCallback<string[] | Dirent[]>
  ): Promise<string[] | Dirent[]> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(options);
      try {
        callback(null, fs.readdirSync(path, options));
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(
        fs.readdirSync(path, options as ReaddirOptions)
      );
    }
  },

  mkdir(
    path: string,
    options?: MkdirOptions | NodeCallback<void>,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      normalizePathLike(path);
      try {
        fs.mkdirSync(path, options);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      fs.mkdirSync(path, options as MkdirOptions);
      return Promise.resolve();
    }
  },

  rmdir(path: string, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      normalizePathLike(path);
      // Defer callback to next tick to allow event loop to process stream events
      const cb = callback;
      try {
        fs.rmdirSync(path);
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e as Error));
      }
    } else {
      return Promise.resolve(fs.rmdirSync(path));
    }
  },

  // rm - remove files or directories (with recursive support)
  rm(
    path: string,
    options?: { force?: boolean; recursive?: boolean } | NodeCallback<void>,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    let opts: { force?: boolean; recursive?: boolean } = {};
    let cb: NodeCallback<void> | undefined;

    if (typeof options === "function") {
      cb = options;
    } else if (options) {
      opts = options;
      cb = callback;
    } else {
      cb = callback;
    }

    const doRm = (): void => {
      try {
        const stats = fs.statSync(path);
        if (stats.isDirectory()) {
          if (opts.recursive) {
            // Recursively remove directory contents
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
        if (opts.force && (e as NodeJS.ErrnoException).code === "ENOENT") {
          return; // Ignore ENOENT when force is true
        }
        throw e;
      }
    };

    if (cb) {
      // Defer callback to next tick to allow event loop to process stream events
      try {
        doRm();
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e as Error));
      }
    } else {
      doRm();
      return Promise.resolve();
    }
  },

  exists(path: string, callback?: (exists: boolean) => void): Promise<boolean> | void {
    validateCallback(callback, "cb");
    if (path === undefined) {
      throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", path);
    }
    queueMicrotask(() => callback(Boolean(tryNormalizeExistsPath(path) && fs.existsSync(path))));
  },

  stat(path: string, callback?: NodeCallback<Stats>): Promise<Stats> | void {
    validateCallback(callback, "cb");
    normalizePathLike(path);
    const cb = callback;
    try {
      const stats = fs.statSync(path);
      queueMicrotask(() => cb(null, stats));
    } catch (e) {
      queueMicrotask(() => cb(e as Error));
    }
  },

  lstat(path: string, callback?: NodeCallback<Stats>): Promise<Stats> | void {
    if (callback) {
      // Defer callback to next tick to allow event loop to process stream events
      const cb = callback;
      try {
        const stats = fs.lstatSync(path);
        queueMicrotask(() => cb(null, stats));
      } catch (e) {
        queueMicrotask(() => cb(e as Error));
      }
    } else {
      return Promise.resolve(fs.lstatSync(path));
    }
  },

  unlink(path: string, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      normalizePathLike(path);
      // Defer callback to next tick to allow event loop to process stream events
      const cb = callback;
      try {
        fs.unlinkSync(path);
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e as Error));
      }
    } else {
      return Promise.resolve(fs.unlinkSync(path));
    }
  },

  rename(
    oldPath: string,
    newPath: string,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    if (callback) {
      normalizePathLike(oldPath, "oldPath");
      normalizePathLike(newPath, "newPath");
      // Defer callback to next tick to allow event loop to process stream events
      const cb = callback;
      try {
        fs.renameSync(oldPath, newPath);
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e as Error));
      }
    } else {
      return Promise.resolve(fs.renameSync(oldPath, newPath));
    }
  },

  copyFile(
    src: string,
    dest: string,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    if (callback) {
      try {
        fs.copyFileSync(src, dest);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.copyFileSync(src, dest));
    }
  },

  cp(
    src: string,
    dest: string,
    options?: { recursive?: boolean; force?: boolean; errorOnExist?: boolean } | NodeCallback<void>,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      try {
        fs.cpSync(src, dest, options as { recursive?: boolean; force?: boolean; errorOnExist?: boolean });
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.cpSync(src, dest, options as { recursive?: boolean; force?: boolean; errorOnExist?: boolean }));
    }
  },

  mkdtemp(
    prefix: string,
    options?: nodeFs.EncodingOption | NodeCallback<string>,
    callback?: NodeCallback<string>
  ): Promise<string> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    validateCallback(callback, "cb");
    validateEncodingOption(options);
    try {
      callback(null, fs.mkdtempSync(prefix, options as nodeFs.EncodingOption));
    } catch (e) {
      callback(e as Error);
    }
  },

  opendir(
    path: string,
    options?: nodeFs.OpenDirOptions | NodeCallback<Dir>,
    callback?: NodeCallback<Dir>
  ): Promise<Dir> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      try {
        callback(null, fs.opendirSync(path, options as nodeFs.OpenDirOptions));
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.opendirSync(path, options as nodeFs.OpenDirOptions));
    }
  },

  open(
    path: string,
    flags?: OpenFlags | NodeCallback<number>,
    mode?: number | NodeCallback<number>,
    callback?: NodeCallback<number>
  ): Promise<number> | void {
    let resolvedFlags: OpenFlags = "r";
    let resolvedMode: number | null | undefined = mode as number | null | undefined;
    if (typeof flags === "function") {
      callback = flags;
      resolvedMode = undefined;
    } else {
      resolvedFlags = flags ?? "r";
    }
    if (typeof mode === "function") {
      callback = mode;
      resolvedMode = undefined;
    }
    validateCallback(callback, "cb");
    normalizePathLike(path);
    normalizeOpenModeArgument(resolvedMode);
    const cb = callback;
    try {
      const fd = fs.openSync(path, resolvedFlags, resolvedMode);
      queueMicrotask(() => cb(null, fd));
    } catch (e) {
      queueMicrotask(() => cb(e as Error));
    }
  },

  close(fd: number, callback?: NodeCallback<void>): Promise<void> | void {
    normalizeFdInteger(fd);
    validateCallback(callback, "cb");
    const cb = callback;
    try {
      fs.closeSync(fd);
      queueMicrotask(() => cb(null));
    } catch (e) {
      queueMicrotask(() => cb(e as Error));
    }
  },

  read(
    fd: number,
    buffer: Uint8Array,
    offset: number,
    length: number,
    position: number | null,
    callback?: (err: Error | null, bytesRead?: number, buffer?: Uint8Array) => void
  ): Promise<number> | void {
    if (callback) {
      // Defer callback to next tick to allow event loop to process stream events
      const cb = callback;
      try {
        const bytesRead = fs.readSync(fd, buffer, offset, length, position);
        queueMicrotask(() => cb(null, bytesRead, buffer));
      } catch (e) {
        queueMicrotask(() => cb(e as Error));
      }
    } else {
      return Promise.resolve(fs.readSync(fd, buffer, offset, length, position));
    }
  },

  write(
    fd: number,
    buffer: string | Uint8Array,
    offset?: number | Record<string, unknown> | NodeCallback<number>,
    length?: number | BufferEncoding | NodeCallback<number>,
    position?: number | null | NodeCallback<number>,
    callback?: NodeCallback<number>
  ): Promise<number> | void {
    if (typeof offset === "function") {
      callback = offset;
      offset = undefined;
      length = undefined;
      position = undefined;
    } else if (typeof length === "function") {
      callback = length;
      length = undefined;
      position = undefined;
    } else if (typeof position === "function") {
      callback = position;
      position = undefined;
    }
    if (callback) {
      const normalized = normalizeWriteSyncArgs(
        buffer,
        offset as number | Record<string, unknown> | null | undefined,
        length as number | BufferEncoding | null | undefined,
        position as number | null | undefined,
      );
      // Defer callback to next tick to allow event loop to process stream events
      const cb = callback;
      try {
        const bytesWritten = typeof normalized.buffer === "string"
          ? _fdWrite.applySyncPromise(
              undefined,
              [fd, Buffer.from(normalized.buffer, normalized.encoding).toString("base64"), normalized.position ?? null],
            )
          : _fdWrite.applySyncPromise(
              undefined,
              [
                fd,
                Buffer.from(
                  new Uint8Array(
                    normalized.buffer.buffer,
                    normalized.buffer.byteOffset + normalized.offset,
                    normalized.length,
                  ),
                ).toString("base64"),
                normalized.position ?? null,
              ],
            );
        queueMicrotask(() => cb(null, bytesWritten));
      } catch (e) {
        queueMicrotask(() => cb(e as Error));
      }
    } else {
      return Promise.resolve(
        fs.writeSync(
          fd,
          buffer,
          offset as number | undefined,
          length as number | undefined,
          position as number | null | undefined
        )
      );
    }
  },

  // writev - write multiple buffers to a file descriptor
  writev(
    fd: number,
    buffers: ArrayBufferView[],
    position?: number | null | ((err: Error | null, bytesWritten?: number, buffers?: ArrayBufferView[]) => void),
    callback?: (err: Error | null, bytesWritten?: number, buffers?: ArrayBufferView[]) => void
  ): void {
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
        queueMicrotask(() => callback(e as Error));
      }
    }
  },

  writevSync(fd: number, buffers: ArrayBufferView[], position?: number | null): number {
    const normalizedFd = normalizeFdInteger(fd);
    const normalizedBuffers = normalizeIoVectorBuffers(buffers);
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

  fstat(fd: number, callback?: NodeCallback<Stats>): Promise<Stats> | void {
    if (callback) {
      try {
        callback(null, fs.fstatSync(fd));
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.fstatSync(fd));
    }
  },

  // fsync / fdatasync async callback forms
  fsync(fd: number, callback?: NodeCallback<void>): Promise<void> | void {
    normalizeFdInteger(fd);
    validateCallback(callback, "cb");
    try {
      fs.fsyncSync(fd);
      callback(null);
    } catch (e) {
      callback(e as Error);
    }
  },

  fdatasync(fd: number, callback?: NodeCallback<void>): Promise<void> | void {
    normalizeFdInteger(fd);
    validateCallback(callback, "cb");
    try {
      fs.fdatasyncSync(fd);
      callback(null);
    } catch (e) {
      callback(e as Error);
    }
  },

  // readv async callback form
  readv(
    fd: number,
    buffers: ArrayBufferView[],
    position?: number | null | ((err: Error | null, bytesRead?: number, buffers?: ArrayBufferView[]) => void),
    callback?: (err: Error | null, bytesRead?: number, buffers?: ArrayBufferView[]) => void
  ): void {
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
        queueMicrotask(() => callback(e as Error));
      }
    }
  },

  // statfs async callback form
  statfs(
    path: PathLike,
    options?: nodeFs.StatFsOptions | NodeCallback<nodeFs.StatsFs>,
    callback?: NodeCallback<nodeFs.StatsFs>
  ): Promise<nodeFs.StatsFs> | void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      try {
        callback(null, fs.statfsSync(path, options as nodeFs.StatFsOptions));
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.statfsSync(path, options as nodeFs.StatFsOptions));
    }
  },

  // glob async callback form
  glob(
    pattern: string | string[],
    options?: nodeFs.GlobOptionsWithFileTypes | ((err: Error | null, matches?: string[]) => void),
    callback?: (err: Error | null, matches?: string[]) => void
  ): void {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (callback) {
      try {
        callback(null, fs.globSync(pattern, options as nodeFs.GlobOptionsWithFileTypes));
      } catch (e) {
        callback(e as Error);
      }
    }
  },

  // fs.promises API
  // Note: Using async functions to properly catch sync errors and return rejected promises
  promises: {
    async readFile(path: string | FileHandle, options?: ReadFileOptions | FileHandleReadFileOptions) {
      if (path instanceof FileHandle) {
        return path.readFile(options as FileHandleReadFileOptions);
      }
      return fs.readFileSync(path, options);
    },
    async writeFile(path: string | FileHandle, data: unknown, options?: WriteFileOptions | FileHandleWriteFileOptions) {
      if (path instanceof FileHandle) {
        return path.writeFile(data, options as FileHandleWriteFileOptions);
      }
      return fs.writeFileSync(path, data as string | Uint8Array, options);
    },
    async appendFile(path: string | FileHandle, data: unknown, options?: WriteFileOptions | FileHandleWriteFileOptions) {
      if (path instanceof FileHandle) {
        return path.appendFile(data, options as FileHandleWriteFileOptions);
      }
      return fs.appendFileSync(path, data as string | Uint8Array, options);
    },
    async readdir(path: string, options?: ReaddirOptions) {
      return fs.readdirSync(path, options);
    },
    async mkdir(path: string, options?: MkdirOptions) {
      return fs.mkdirSync(path, options);
    },
    async rmdir(path: string) {
      return fs.rmdirSync(path);
    },
    async stat(path: string) {
      return fs.statSync(path);
    },
    async lstat(path: string) {
      return fs.lstatSync(path);
    },
    async unlink(path: string) {
      return fs.unlinkSync(path);
    },
    async rename(oldPath: string, newPath: string) {
      return fs.renameSync(oldPath, newPath);
    },
    async copyFile(src: string, dest: string) {
      return fs.copyFileSync(src, dest);
    },
    async cp(src: string, dest: string, options?: { recursive?: boolean; force?: boolean; errorOnExist?: boolean }) {
      return fs.cpSync(src, dest, options);
    },
    async mkdtemp(prefix: string, options?: nodeFs.EncodingOption) {
      return fs.mkdtempSync(prefix, options);
    },
    async opendir(path: string, options?: nodeFs.OpenDirOptions) {
      return fs.opendirSync(path, options);
    },
    async open(path: string, flags?: OpenFlags, mode?: Mode): Promise<FileHandle> {
      return new FileHandle(fs.openSync(path, flags ?? "r", mode));
    },
    async statfs(path: string, options?: nodeFs.StatFsOptions) {
      return fs.statfsSync(path, options);
    },
    async glob(pattern: string | string[], _options?: nodeFs.GlobOptionsWithFileTypes) {
      return fs.globSync(pattern, _options);
    },
    async access(path: string) {
      if (!fs.existsSync(path)) {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, access '${path}'`,
          "access",
          path
        );
      }
    },
    async rm(path: string, options?: { force?: boolean; recursive?: boolean }) {
      return fs.rmSync(path, options);
    },
    async chmod(path: string, mode: Mode): Promise<void> {
      return fs.chmodSync(path, mode);
    },
    async chown(path: string, uid: number, gid: number): Promise<void> {
      return fs.chownSync(path, uid, gid);
    },
    async lchown(path: string, uid: number, gid: number): Promise<void> {
      return fs.lchownSync(path, uid, gid);
    },
    async link(existingPath: string, newPath: string): Promise<void> {
      return fs.linkSync(existingPath, newPath);
    },
    async symlink(target: string, path: string): Promise<void> {
      return fs.symlinkSync(target, path);
    },
    async readlink(path: string): Promise<string> {
      return fs.readlinkSync(path);
    },
    async truncate(path: string, len?: number): Promise<void> {
      return fs.truncateSync(path, len);
    },
    async utimes(path: string, atime: string | number | Date, mtime: string | number | Date): Promise<void> {
      return fs.utimesSync(path, atime, mtime);
    },
    watch(path: unknown, options?: unknown) {
      return createUnsupportedPromisesWatchIterator(path, options);
    },
  },

  // Compatibility methods

  accessSync(path: string): void {
    // existsSync already normalizes the path
    if (!fs.existsSync(path)) {
      throw createFsError(
        "ENOENT",
        `ENOENT: no such file or directory, access '${path}'`,
        "access",
        path
      );
    }
  },

  access(
    path: string,
    mode?: number | NodeCallback<void>,
    callback?: NodeCallback<void>
  ): Promise<void> | void {
    if (typeof mode === "function") {
      callback = mode;
      mode = undefined;
    }
    if (callback) {
      try {
        fs.accessSync(path);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return fs.promises.access(path);
    }
  },

  realpathSync: Object.assign(
    function realpathSync(path: PathLike, options?: nodeFs.EncodingOption): string {
      validateEncodingOption(options);
      // Resolve symlinks by walking each path component via lstat + readlink
      const MAX_SYMLINK_DEPTH = 40;
      let symlinksFollowed = 0;
      const raw = normalizePathLike(path);

      // Build initial queue: normalize . and .. segments
      const pending: string[] = [];
      for (const seg of raw.split("/")) {
        if (!seg || seg === ".") continue;
        if (seg === "..") { if (pending.length > 0) pending.pop(); }
        else pending.push(seg);
      }

      // Walk each component, resolving symlinks via a queue
      const resolved: string[] = [];
      while (pending.length > 0) {
        const seg = pending.shift()!;
        if (seg === ".") continue;
        if (seg === "..") { if (resolved.length > 0) resolved.pop(); continue; }
        resolved.push(seg);
        const currentPath = "/" + resolved.join("/");
        try {
          const stat = fs.lstatSync(currentPath);
          if (stat.isSymbolicLink()) {
            if (++symlinksFollowed > MAX_SYMLINK_DEPTH) {
              const err = new Error(`ELOOP: too many levels of symbolic links, realpath '${raw}'`) as NodeJS.ErrnoException;
              err.code = "ELOOP";
              err.syscall = "realpath";
              err.path = raw;
              throw err;
            }
            const target = fs.readlinkSync(currentPath);
            // Prepend target segments to pending for re-resolution
            const targetSegs = target.split("/").filter(Boolean);
            if (target.startsWith("/")) {
              // Absolute symlink — restart from root
              resolved.length = 0;
            } else {
              // Relative symlink — drop current component
              resolved.pop();
            }
            // Prepend target segments so they're processed next
            pending.unshift(...targetSegs);
          }
        } catch (e: unknown) {
          const err = e as NodeJS.ErrnoException;
          if (err.code === "ELOOP") throw e;
          if (err.code === "ENOENT" || err.code === "ENOTDIR") {
            const enoent = new Error(`ENOENT: no such file or directory, realpath '${raw}'`) as NodeJS.ErrnoException;
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
      native(path: PathLike, options?: nodeFs.EncodingOption): string {
        validateEncodingOption(options);
        return fs.realpathSync(path);
      }
    }
  ),

  realpath: Object.assign(
    function realpath(
      path: PathLike,
      optionsOrCallback?: nodeFs.EncodingOption | NodeCallback<string>,
      callback?: NodeCallback<string>,
    ): Promise<string> | void {
      let options: nodeFs.EncodingOption | undefined;
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
      native(
        path: PathLike,
        optionsOrCallback?: nodeFs.EncodingOption | NodeCallback<string>,
        callback?: NodeCallback<string>,
      ): Promise<string> | void {
        let options: nodeFs.EncodingOption | undefined;
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

  createReadStream: function createReadStream(
    path: nodeFs.PathLike,
    options?: BufferEncoding | {
      encoding?: BufferEncoding;
      start?: number;
      end?: number;
      highWaterMark?: number;
      bufferSize?: number;
      autoClose?: boolean;
      fd?: number | FileHandle;
      fs?: unknown;
      signal?: AbortSignal;
    }
  ): nodeFs.ReadStream {
    const opts = typeof options === "string" ? { encoding: options } : options;
    validateEncodingOption(opts);
    const fd = normalizeStreamFd(opts?.fd);
    const pathLike = normalizeStreamPath(path as nodeFs.PathLike | null, fd);
    // Use type assertion since our ReadStream has all the methods npm needs
    // but not all the complex overloaded signatures of the full Node.js interface
    return new ReadStream(pathLike, opts) as unknown as nodeFs.ReadStream;
  },

  createWriteStream: function createWriteStream(
    path: nodeFs.PathLike,
    options?: BufferEncoding | {
      encoding?: BufferEncoding;
      flags?: string;
      mode?: number;
      autoClose?: boolean;
      fd?: number | FileHandle;
      fs?: unknown;
    }
  ): nodeFs.WriteStream {
    const opts = typeof options === "string" ? { encoding: options } : options;
    validateEncodingOption(opts);
    validateWriteStreamStartOption((opts ?? {}) as Record<string, unknown>);
    const fd = normalizeStreamFd(opts?.fd);
    const pathLike = normalizeStreamPath(path as nodeFs.PathLike | null, fd);
    // Use type assertion since our WriteStream has all the methods npm needs
    // but not all the complex overloaded signatures of the full Node.js interface
    return new WriteStream(pathLike, opts) as unknown as nodeFs.WriteStream;
  },

  // Unsupported fs APIs — watch requires kernel-level inotify, use polling instead
  watch(..._args: unknown[]): never {
    normalizeWatchArguments(_args[0], _args[1], _args[2]);
    throw createUnsupportedWatcherError("watch");
  },

  watchFile(..._args: unknown[]): never {
    normalizeWatchFileArguments(_args[0], _args[1], _args[2]);
    throw createUnsupportedWatcherError("watchFile");
  },

  unwatchFile(..._args: unknown[]): never {
    normalizePathLike(_args[0]);
    throw createUnsupportedWatcherError("unwatchFile");
  },

  chmod(path: PathLike, mode: Mode, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      normalizePathLike(path);
      normalizeModeArgument(mode);
      try {
        fs.chmodSync(path, mode);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.chmodSync(path, mode));
    }
  },

  chown(path: PathLike, uid: number, gid: number, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      normalizePathLike(path);
      normalizeNumberArgument("uid", uid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      try {
        fs.chownSync(path, uid, gid);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.chownSync(path, uid, gid));
    }
  },

  fchmod(fd: number, mode: Mode, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      normalizeFdInteger(fd);
      normalizeModeArgument(mode);
      try {
        fs.fchmodSync(fd, mode);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      normalizeFdInteger(fd);
      normalizeModeArgument(mode);
      return Promise.resolve(fs.fchmodSync(fd, mode));
    }
  },

  fchown(fd: number, uid: number, gid: number, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      normalizeFdInteger(fd);
      normalizeNumberArgument("uid", uid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      try {
        fs.fchownSync(fd, uid, gid);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      normalizeFdInteger(fd);
      normalizeNumberArgument("uid", uid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      return Promise.resolve(fs.fchownSync(fd, uid, gid));
    }
  },

  lchown(path: PathLike, uid: number, gid: number, callback?: NodeCallback<void>): Promise<void> | void {
    if (arguments.length >= 4) {
      validateCallback(callback, "cb");
      normalizePathLike(path);
      normalizeNumberArgument("uid", uid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      normalizeNumberArgument("gid", gid, { min: -1, max: 0xffffffff, allowNegativeOne: true });
      try {
        fs.lchownSync(path, uid, gid);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.lchownSync(path, uid, gid));
    }
  },

  link(existingPath: PathLike, newPath: PathLike, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      normalizePathLike(existingPath, "existingPath");
      normalizePathLike(newPath, "newPath");
      try {
        fs.linkSync(existingPath, newPath);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.linkSync(existingPath, newPath));
    }
  },

  symlink(target: PathLike, path: PathLike, typeOrCb?: string | null | NodeCallback<void>, callback?: NodeCallback<void>): Promise<void> | void {
    if (typeof typeOrCb === "function") {
      callback = typeOrCb;
    }
    if (callback) {
      try {
        fs.symlinkSync(target, path);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.symlinkSync(target, path));
    }
  },

  readlink(path: PathLike, optionsOrCb?: nodeFs.EncodingOption | NodeCallback<string>, callback?: NodeCallback<string>): Promise<string> | void {
    if (typeof optionsOrCb === "function") {
      callback = optionsOrCb;
      optionsOrCb = undefined;
    }
    if (callback) {
      normalizePathLike(path);
      validateEncodingOption(optionsOrCb);
      try {
        callback(null, fs.readlinkSync(path, optionsOrCb));
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.readlinkSync(path, optionsOrCb));
    }
  },

  truncate(path: PathLike, lenOrCb?: number | null | NodeCallback<void>, callback?: NodeCallback<void>): Promise<void> | void {
    if (typeof lenOrCb === "function") {
      callback = lenOrCb;
      lenOrCb = 0;
    }
    if (callback) {
      try {
        fs.truncateSync(path, lenOrCb as number | null);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.truncateSync(path, lenOrCb as number | null));
    }
  },

  utimes(path: PathLike, atime: string | number | Date, mtime: string | number | Date, callback?: NodeCallback<void>): Promise<void> | void {
    if (callback) {
      try {
        fs.utimesSync(path, atime, mtime);
        callback(null);
      } catch (e) {
        callback(e as Error);
      }
    } else {
      return Promise.resolve(fs.utimesSync(path, atime, mtime));
    }
  },
};

// Wire late-bound glob helpers to the fs object
_globReadDir = (dir: string) => fs.readdirSync(dir) as string[];
_globStat = (path: string) => fs.statSync(path);

// Export the fs module
export default fs;
