// Process module polyfill for the sandbox
// Provides Node.js process object and global polyfills for sandbox compatibility

import type * as nodeProcess from "process";

// Re-export WHATWG globals from polyfills (polyfills.ts is imported first in index.ts)
import {
	TextEncoder,
	TextDecoder,
	Event,
	CustomEvent,
	EventTarget,
} from "./polyfills.js";

import {
	URL,
	URLSearchParams,
	installWhatwgUrlGlobals,
} from "./whatwg-url.js";

// Use buffer package for spec-compliant Buffer implementation
import { Buffer as BufferPolyfill } from "buffer";
import type {
	CryptoRandomFillBridgeRef,
	CryptoRandomUuidBridgeRef,
	CryptoSubtleBridgeRef,
	FsFacadeBridge,
	KernelStdinReadBridgeRef,
	ProcessErrorBridgeRef,
	ProcessLogBridgeRef,
	PtySetRawModeBridgeRef,
} from "../bridge-contract.js";
import {
  exposeCustomGlobal,
  exposeMutableRuntimeStateGlobal,
} from "@secure-exec/core/internal/shared/global-exposure";
import { bridgeDispatchSync } from "./dispatch.js";


/**
 * Process configuration injected by the host before the bridge bundle loads.
 * Values default to sensible Linux/x64 stubs when unset.
 */
export interface ProcessConfig {
  platform?: string;
  arch?: string;
  version?: string;
  cwd?: string;
  env?: Record<string, string>;
  argv?: string[];
  execPath?: string;
  pid?: number;
  ppid?: number;
  uid?: number;
  gid?: number;
  stdin?: string;
  timingMitigation?: "off" | "freeze";
  frozenTimeMs?: number;
  stdinIsTTY?: boolean;
  stdoutIsTTY?: boolean;
  stderrIsTTY?: boolean;
  /** Terminal columns (from PTY dimensions). */
  cols?: number;
  /** Terminal rows (from PTY dimensions). */
  rows?: number;
}

// Declare config and host bridge globals
declare const _processConfig: ProcessConfig | undefined;
declare const _log: ProcessLogBridgeRef;
declare const _error: ProcessErrorBridgeRef;
declare const _cryptoRandomFill: CryptoRandomFillBridgeRef | undefined;
declare const _cryptoRandomUUID: CryptoRandomUuidBridgeRef | undefined;
declare const _cryptoSubtle: CryptoSubtleBridgeRef | undefined;
// Filesystem bridge for chdir validation
declare const _fs: FsFacadeBridge;
// PTY setRawMode bridge ref (optional — only present when PTY is attached)
declare const _ptySetRawMode: PtySetRawModeBridgeRef | undefined;
declare const _kernelStdinRead: KernelStdinReadBridgeRef | undefined;
declare const _registerHandle:
  | ((id: string, description: string) => void)
  | undefined;
declare const _unregisterHandle:
  | ((id: string) => void)
  | undefined;

// Get config with defaults
function readProcessConfig() {
  return {
    platform:
      (typeof _processConfig !== "undefined" && _processConfig.platform) ||
      "linux",
    arch:
      (typeof _processConfig !== "undefined" && _processConfig.arch) || "x64",
    version:
      (typeof _processConfig !== "undefined" && _processConfig.version) ||
      "v22.0.0",
    cwd: (typeof _processConfig !== "undefined" && _processConfig.cwd) || "/root",
    env: (typeof _processConfig !== "undefined" && _processConfig.env) || {},
    argv:
      (typeof _processConfig !== "undefined" && _processConfig.argv) || [
        "node",
        "script.js",
      ],
    execPath:
      (typeof _processConfig !== "undefined" && _processConfig.execPath) ||
      "/usr/bin/node",
    pid:
      (typeof _processConfig !== "undefined" && _processConfig.pid) || 1,
    ppid:
      (typeof _processConfig !== "undefined" && _processConfig.ppid) || 0,
    uid:
      (typeof _processConfig !== "undefined" && _processConfig.uid) || 0,
    gid:
      (typeof _processConfig !== "undefined" && _processConfig.gid) || 0,
    stdin:
      (typeof _processConfig !== "undefined" ? _processConfig.stdin : undefined),
    timingMitigation:
      (typeof _processConfig !== "undefined" && _processConfig.timingMitigation) ||
      "off",
    frozenTimeMs:
      typeof _processConfig !== "undefined" ? _processConfig.frozenTimeMs : undefined,
  };
}

let config = readProcessConfig();

/** Get the current timestamp, returning a frozen value when timing mitigation is active. */
function getNowMs(): number {
  if (
    config.timingMitigation === "freeze" &&
    typeof config.frozenTimeMs === "number"
  ) {
    return config.frozenTimeMs;
  }
  return typeof performance !== "undefined" && performance.now
    ? performance.now()
    : Date.now();
}

// Start time for uptime calculation
const _processStartTime = getNowMs();

const BUFFER_MAX_LENGTH =
  typeof (BufferPolyfill as unknown as { kMaxLength?: unknown }).kMaxLength ===
  "number"
    ? ((BufferPolyfill as unknown as { kMaxLength: number }).kMaxLength as number)
    : 2147483647;
const BUFFER_MAX_STRING_LENGTH =
  typeof (BufferPolyfill as unknown as { kStringMaxLength?: unknown }).kStringMaxLength ===
  "number"
    ? ((BufferPolyfill as unknown as { kStringMaxLength: number }).kStringMaxLength as number)
    : 536870888;
const BUFFER_CONSTANTS = Object.freeze({
  MAX_LENGTH: BUFFER_MAX_LENGTH,
  MAX_STRING_LENGTH: BUFFER_MAX_STRING_LENGTH,
});

const bufferPolyfillMutable = BufferPolyfill as unknown as {
  kMaxLength?: number;
  kStringMaxLength?: number;
  constants?: Record<string, number>;
};
if (typeof bufferPolyfillMutable.kMaxLength !== "number") {
  bufferPolyfillMutable.kMaxLength = BUFFER_MAX_LENGTH;
}
if (typeof bufferPolyfillMutable.kStringMaxLength !== "number") {
  bufferPolyfillMutable.kStringMaxLength = BUFFER_MAX_STRING_LENGTH;
}
if (
  typeof bufferPolyfillMutable.constants !== "object" ||
  bufferPolyfillMutable.constants === null
) {
  bufferPolyfillMutable.constants = {
    MAX_LENGTH: BUFFER_MAX_LENGTH,
    MAX_STRING_LENGTH: BUFFER_MAX_STRING_LENGTH,
  };
}

// Shim encoding-specific slice/write methods on Buffer.prototype.
// Node.js exposes these via internal V8 bindings (e.g. utf8Slice, latin1Write).
// Packages like ssh2 call them directly for performance.
const bufferProto = BufferPolyfill.prototype as Record<string, unknown>;
if (typeof bufferProto.utf8Slice !== "function") {
  const encodings = ["utf8", "latin1", "ascii", "hex", "base64", "ucs2", "utf16le"];
  for (const enc of encodings) {
    if (typeof bufferProto[enc + "Slice"] !== "function") {
      bufferProto[enc + "Slice"] = function (this: InstanceType<typeof BufferPolyfill>, start?: number, end?: number) {
        return this.toString(enc as BufferEncoding, start, end);
      };
    }
    if (typeof bufferProto[enc + "Write"] !== "function") {
      bufferProto[enc + "Write"] = function (this: InstanceType<typeof BufferPolyfill>, string: string, offset?: number, length?: number) {
        return this.write(string, offset ?? 0, length ?? (this.length - (offset ?? 0)), enc as BufferEncoding);
      };
    }
  }
}

const bufferCtorMutable = BufferPolyfill as typeof BufferPolyfill & {
  allocUnsafe?: typeof BufferPolyfill.allocUnsafe & { _secureExecPatched?: boolean };
};
if (
  typeof bufferCtorMutable.allocUnsafe === "function" &&
  !bufferCtorMutable.allocUnsafe._secureExecPatched
) {
  const originalAllocUnsafe = bufferCtorMutable.allocUnsafe;
  bufferCtorMutable.allocUnsafe = function patchedAllocUnsafe(
    this: typeof BufferPolyfill,
    size: number,
  ): Buffer {
    try {
      return originalAllocUnsafe.call(this, size);
    } catch (error) {
      if (
        error instanceof RangeError &&
        typeof size === "number" &&
        size > BUFFER_MAX_LENGTH
      ) {
        throw new Error("Array buffer allocation failed");
      }
      throw error;
    }
  } as typeof BufferPolyfill.allocUnsafe & { _secureExecPatched?: boolean };
  bufferCtorMutable.allocUnsafe._secureExecPatched = true;
}

// Exit code tracking
let _exitCode = 0;
let _exited = false;

/**
 * Thrown by `process.exit()` to unwind the sandbox call stack. The host
 * catches this to extract the exit code without killing the isolate.
 */
export class ProcessExitError extends Error {
  code: number;
  _isProcessExit: true;
  constructor(code: number) {
    super("process.exit(" + code + ")");
    this.name = "ProcessExitError";
    this.code = code;
    this._isProcessExit = true;
  }
}

// Make available globally
exposeCustomGlobal("ProcessExitError", ProcessExitError);

// Signal name → number mapping (POSIX standard)
const _signalNumbers: Record<string, number> = {
  SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGILL: 4, SIGTRAP: 5, SIGABRT: 6,
  SIGBUS: 7, SIGFPE: 8, SIGKILL: 9, SIGUSR1: 10, SIGSEGV: 11, SIGUSR2: 12,
  SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15, SIGCHLD: 17, SIGCONT: 18,
  SIGSTOP: 19, SIGTSTP: 20, SIGTTIN: 21, SIGTTOU: 22, SIGURG: 23,
  SIGXCPU: 24, SIGXFSZ: 25, SIGVTALRM: 26, SIGPROF: 27, SIGWINCH: 28,
  SIGIO: 29, SIGPWR: 30, SIGSYS: 31,
};
const _signalNamesByNumber: Record<number, string> = Object.fromEntries(
  Object.entries(_signalNumbers).map(([name, num]) => [num, name])
) as Record<number, string>;
const _ignoredSelfSignals = new Set(["SIGWINCH", "SIGCHLD", "SIGCONT", "SIGURG"]);

function _resolveSignal(signal?: string | number): number {
  if (signal === undefined || signal === null) return 15; // default SIGTERM
  if (typeof signal === "number") return signal;
  const num = _signalNumbers[signal];
  if (num !== undefined) return num;
  throw new Error("Unknown signal: " + signal);
}

// EventEmitter implementation for process
type EventListener = (...args: unknown[]) => void;
const _processListeners: Record<string, EventListener[]> = {};
const _processOnceListeners: Record<string, EventListener[]> = {};
let _processMaxListeners = 10;
const _processMaxListenersWarned = new Set<string>();

function _addListener(
  event: string,
  listener: EventListener,
  once = false
): unknown {
  const target = once ? _processOnceListeners : _processListeners;
  if (!target[event]) {
    target[event] = [];
  }
  target[event].push(listener);

  // Warn when exceeding maxListeners (Node.js behavior: warn, don't crash)
  if (_processMaxListeners > 0 && !_processMaxListenersWarned.has(event)) {
    const total = (_processListeners[event]?.length ?? 0) + (_processOnceListeners[event]?.length ?? 0);
    if (total > _processMaxListeners) {
      _processMaxListenersWarned.add(event);
      const warning = `MaxListenersExceededWarning: Possible EventEmitter memory leak detected. ${total} ${event} listeners added to [process]. MaxListeners is ${_processMaxListeners}. Use emitter.setMaxListeners() to increase limit`;
      // Use console.error to emit warning without recursion risk
      if (typeof _error !== "undefined") {
        _error.applySync(undefined, [warning]);
      }
    }
  }

  return process;
}

function _removeListener(
  event: string,
  listener: EventListener
): unknown {
  if (_processListeners[event]) {
    const idx = _processListeners[event].indexOf(listener);
    if (idx !== -1) _processListeners[event].splice(idx, 1);
  }
  if (_processOnceListeners[event]) {
    const idx = _processOnceListeners[event].indexOf(listener);
    if (idx !== -1) _processOnceListeners[event].splice(idx, 1);
  }
  return process;
}

function _emit(event: string, ...args: unknown[]): boolean {
  let handled = false;

  // Regular listeners
  if (_processListeners[event]) {
    for (const listener of _processListeners[event]) {
      listener(...args);
      handled = true;
    }
  }

  // Once listeners (remove after calling)
  if (_processOnceListeners[event]) {
    const listeners = _processOnceListeners[event].slice();
    _processOnceListeners[event] = [];
    for (const listener of listeners) {
      listener(...args);
      handled = true;
    }
  }

  return handled;
}

function isProcessExitError(error: unknown): error is { code?: unknown } {
  return Boolean(
    error &&
      typeof error === "object" &&
      (
        (error as { _isProcessExit?: unknown })._isProcessExit === true ||
        (error as { name?: unknown }).name === "ProcessExitError"
      ),
  );
}

function normalizeAsyncError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function routeAsyncCallbackError(error: unknown): { handled: boolean; rethrow: unknown | null } {
  if (isProcessExitError(error)) {
    return { handled: false, rethrow: error };
  }

  const normalized = normalizeAsyncError(error);

  try {
    if (_emit("uncaughtException", normalized, "uncaughtException")) {
      return { handled: true, rethrow: null };
    }
  } catch (emitError) {
    return { handled: false, rethrow: emitError };
  }

  return { handled: false, rethrow: normalized };
}

function scheduleAsyncRethrow(error: unknown): void {
  setTimeout(() => {
    throw error;
  }, 0);
}

// Stdio stream shape shared by stdout and stderr
interface StdioWriteStream {
  write(data: unknown, encodingOrCallback?: unknown, callback?: unknown): boolean;
  end(): StdioWriteStream;
  on(event: string, listener: EventListener): StdioWriteStream;
  once(event: string, listener: EventListener): StdioWriteStream;
  off(event: string, listener: EventListener): StdioWriteStream;
  removeListener(event: string, listener: EventListener): StdioWriteStream;
  addListener(event: string, listener: EventListener): StdioWriteStream;
  emit(event: string, ...args: unknown[]): boolean;
  writable: boolean;
  isTTY: boolean;
  columns: number;
  rows: number;
}

// Lazy TTY flag readers — __runtimeTtyConfig is set by postRestoreScript
// (cannot use _processConfig because InjectGlobals overwrites it later)
declare const __runtimeTtyConfig: { stdinIsTTY?: boolean; stdoutIsTTY?: boolean; stderrIsTTY?: boolean; cols?: number; rows?: number } | undefined;
function _getStdinIsTTY(): boolean {
  return (typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.stdinIsTTY) || false;
}
function _getStdoutIsTTY(): boolean {
  return (typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.stdoutIsTTY) || false;
}
function _getStderrIsTTY(): boolean {
  return (typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.stderrIsTTY) || false;
}

function getWriteCallback(
  encodingOrCallback?: unknown,
  callback?: unknown,
): ((error?: Error | null) => void) | undefined {
  if (typeof encodingOrCallback === "function") {
    return encodingOrCallback as (error?: Error | null) => void;
  }
  if (typeof callback === "function") {
    return callback as (error?: Error | null) => void;
  }
  return undefined;
}

function emitListeners(
  listeners: Record<string, EventListener[]>,
  onceListeners: Record<string, EventListener[]>,
  event: string,
  args: unknown[],
): boolean {
  const persistent = listeners[event] ? listeners[event].slice() : [];
  const once = onceListeners[event] ? onceListeners[event].slice() : [];
  if (once.length > 0) {
    onceListeners[event] = [];
  }
  for (const listener of persistent) {
    listener(...args);
  }
  for (const listener of once) {
    listener(...args);
  }
  return persistent.length + once.length > 0;
}

function createStdioWriteStream(options: {
  write(data: string): void;
  isTTY: () => boolean;
}): StdioWriteStream {
  const listeners: Record<string, EventListener[]> = {};
  const onceListeners: Record<string, EventListener[]> = {};

  const remove = (event: string, listener: EventListener): void => {
    if (listeners[event]) {
      const idx = listeners[event].indexOf(listener);
      if (idx !== -1) listeners[event].splice(idx, 1);
    }
    if (onceListeners[event]) {
      const idx = onceListeners[event].indexOf(listener);
      if (idx !== -1) onceListeners[event].splice(idx, 1);
    }
  };

  const decoder = new TextDecoder();

  const stream: StdioWriteStream = {
    write(data: unknown, encodingOrCallback?: unknown, callback?: unknown): boolean {
      // Handle Uint8Array/Buffer data by decoding to UTF-8 (matches Node.js behavior)
      if (data instanceof Uint8Array || (typeof BufferPolyfill !== "undefined" && BufferPolyfill.isBuffer(data))) {
        options.write(decoder.decode(data as Uint8Array));
      } else {
        options.write(String(data));
      }
      const done = getWriteCallback(encodingOrCallback, callback);
      if (done) {
        _queueMicrotask(() => done(null));
      }
      return true;
    },
    end(): StdioWriteStream {
      return stream;
    },
    on(event: string, listener: EventListener): StdioWriteStream {
      if (!listeners[event]) listeners[event] = [];
      listeners[event].push(listener);
      return stream;
    },
    once(event: string, listener: EventListener): StdioWriteStream {
      if (!onceListeners[event]) onceListeners[event] = [];
      onceListeners[event].push(listener);
      return stream;
    },
    off(event: string, listener: EventListener): StdioWriteStream {
      remove(event, listener);
      return stream;
    },
    removeListener(event: string, listener: EventListener): StdioWriteStream {
      remove(event, listener);
      return stream;
    },
    addListener(event: string, listener: EventListener): StdioWriteStream {
      return stream.on(event, listener);
    },
    emit(event: string, ...args: unknown[]): boolean {
      return emitListeners(listeners, onceListeners, event, args);
    },
    writable: true,
    get isTTY(): boolean { return options.isTTY(); },
    get columns(): number {
      return (typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.cols) || 80;
    },
    get rows(): number {
      return (typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.rows) || 24;
    },
  };

  return stream;
}

const _stdout = createStdioWriteStream({
	write(data: string): void {
		if (typeof _log !== "undefined") {
			_log.applySync(undefined, [data]);
		}
	},
  isTTY: _getStdoutIsTTY,
});

const _stderr = createStdioWriteStream({
  write(data: string): void {
    if (typeof _error !== "undefined") {
      _error.applySync(undefined, [data]);
    }
  },
  isTTY: _getStderrIsTTY,
});

// Stdin stream with data support
// These are exposed as globals so they can be set after bridge initialization
type StdinListener = (data?: unknown) => void;
const _stdinListeners: Record<string, StdinListener[]> = {};
const _stdinOnceListeners: Record<string, StdinListener[]> = {};
let _stdinLiveDecoder = new TextDecoder();
const STDIN_HANDLE_ID = "process.stdin";
let _stdinLiveBuffer = "";
let _stdinLiveStarted = false;
let _stdinLiveHandleRegistered = false;

// Initialize stdin state as globals for external access
exposeMutableRuntimeStateGlobal(
  "_stdinData",
  (typeof _processConfig !== "undefined" && _processConfig.stdin) || "",
);
exposeMutableRuntimeStateGlobal("_stdinPosition", 0);
exposeMutableRuntimeStateGlobal("_stdinEnded", false);
exposeMutableRuntimeStateGlobal("_stdinFlowMode", false);

// Getters for the globals
function getStdinData(): string { return (globalThis as Record<string, unknown>)._stdinData as string; }
function setStdinDataValue(v: string): void { (globalThis as Record<string, unknown>)._stdinData = v; }
function getStdinPosition(): number { return (globalThis as Record<string, unknown>)._stdinPosition as number; }
function setStdinPosition(v: number): void { (globalThis as Record<string, unknown>)._stdinPosition = v; }
function getStdinEnded(): boolean { return (globalThis as Record<string, unknown>)._stdinEnded as boolean; }
function setStdinEnded(v: boolean): void { (globalThis as Record<string, unknown>)._stdinEnded = v; }
function getStdinFlowMode(): boolean { return (globalThis as Record<string, unknown>)._stdinFlowMode as boolean; }
function setStdinFlowMode(v: boolean): void { (globalThis as Record<string, unknown>)._stdinFlowMode = v; }

function _emitStdinData(): void {
  if (getStdinEnded() || !getStdinData()) return;

  // In flowing mode, emit all remaining data
  if (getStdinFlowMode() && getStdinPosition() < getStdinData().length) {
    const chunk = getStdinData().slice(getStdinPosition());
    setStdinPosition(getStdinData().length);

    // Emit data event
    const dataListeners = [...(_stdinListeners["data"] || []), ...(_stdinOnceListeners["data"] || [])];
    _stdinOnceListeners["data"] = [];
    for (const listener of dataListeners) {
      listener(chunk);
    }

    // Emit end after all data
    setStdinEnded(true);
    const endListeners = [...(_stdinListeners["end"] || []), ...(_stdinOnceListeners["end"] || [])];
    _stdinOnceListeners["end"] = [];
    for (const listener of endListeners) {
      listener();
    }

    // Emit close
    const closeListeners = [...(_stdinListeners["close"] || []), ...(_stdinOnceListeners["close"] || [])];
    _stdinOnceListeners["close"] = [];
    for (const listener of closeListeners) {
      listener();
    }
  }
}

function emitStdinListeners(event: string, value?: unknown): void {
  const listeners = [...(_stdinListeners[event] || []), ...(_stdinOnceListeners[event] || [])];
  _stdinOnceListeners[event] = [];
  for (const listener of listeners) {
    listener(value);
  }
}

function syncLiveStdinHandle(active: boolean): void {
  if (active) {
    if (!_stdinLiveHandleRegistered && typeof _registerHandle === "function") {
      try {
        _registerHandle(STDIN_HANDLE_ID, "process.stdin");
        _stdinLiveHandleRegistered = true;
      } catch {
        // Process exit races turn registration into a no-op.
      }
    }
    return;
  }

  if (_stdinLiveHandleRegistered && typeof _unregisterHandle === "function") {
    try {
      _unregisterHandle(STDIN_HANDLE_ID);
    } catch {
      // Process exit races turn unregistration into a no-op.
    }
    _stdinLiveHandleRegistered = false;
  }
}

function flushLiveStdinBuffer(): void {
  if (!getStdinFlowMode() || _stdinLiveBuffer.length === 0) return;
  const chunk = _stdinLiveBuffer;
  _stdinLiveBuffer = "";
  // Emit as Buffer when no encoding is set (matches real Node.js process.stdin)
  const data = (_stdin as StdinStream).encoding
    ? chunk
    : BufferPolyfill.from(chunk);
  emitStdinListeners("data", data);
}

function finishLiveStdin(): void {
  if (getStdinEnded()) return;
  setStdinEnded(true);
  flushLiveStdinBuffer();
  emitStdinListeners("end");
  emitStdinListeners("close");
  syncLiveStdinHandle(false);
}

declare const __runtimeStreamStdin: boolean | undefined;
function _getStreamStdin(): boolean {
  return typeof __runtimeStreamStdin !== "undefined" && !!__runtimeStreamStdin;
}

function ensureLiveStdinStarted(): void {
  if (_stdinLiveStarted) return;
  if (!_getStdinIsTTY() && !_getStreamStdin()) return;
  if (typeof _kernelStdinRead === "undefined") return;
  _stdinLiveStarted = true;
  syncLiveStdinHandle(!(_stdin as StdinStream).paused);
  void (async () => {
    try {
      while (!getStdinEnded()) {
        if (typeof _kernelStdinRead === "undefined") {
          break;
        }
        const next = await _kernelStdinRead.apply(undefined, [], {
          result: { promise: true },
        });
        if (next?.done) {
          break;
        }

        const dataBase64 = String(next?.dataBase64 ?? "");
        if (!dataBase64) {
          continue;
        }

        _stdinLiveBuffer += _stdinLiveDecoder.decode(
          BufferPolyfill.from(dataBase64, "base64"),
          { stream: true },
        );
        flushLiveStdinBuffer();
      }
    } catch {
      // Treat bridge-side stdin failures as EOF for sandbox code.
    }

    _stdinLiveBuffer += _stdinLiveDecoder.decode();
    finishLiveStdin();
  })();
}

// Stdin stream shape
interface StdinStream {
  readable: boolean;
  paused: boolean;
  encoding: string | null;
  isRaw: boolean;
  read(size?: number): string | null;
  on(event: string, listener: StdinListener): StdinStream;
  once(event: string, listener: StdinListener): StdinStream;
  off(event: string, listener: StdinListener): StdinStream;
  removeListener(event: string, listener: StdinListener): StdinStream;
  emit(event: string, ...args: unknown[]): boolean;
  pause(): StdinStream;
  resume(): StdinStream;
  setEncoding(enc: string): StdinStream;
  setRawMode(mode: boolean): StdinStream;
  isTTY: boolean;
  [Symbol.asyncIterator]: () => AsyncGenerator<string, void, unknown>;
}

const _stdin: StdinStream = {
  readable: true,
  paused: true,
  encoding: null as string | null,
  isRaw: false,

  read(size?: number): string | null {
    if (_stdinLiveBuffer.length > 0) {
      if (!size || size >= _stdinLiveBuffer.length) {
        const chunk = _stdinLiveBuffer;
        _stdinLiveBuffer = "";
        return chunk;
      }
      const chunk = _stdinLiveBuffer.slice(0, size);
      _stdinLiveBuffer = _stdinLiveBuffer.slice(size);
      return chunk;
    }
    if (getStdinPosition() >= getStdinData().length) return null;
    const chunk = size ? getStdinData().slice(getStdinPosition(), getStdinPosition() + size) : getStdinData().slice(getStdinPosition());
    setStdinPosition(getStdinPosition() + chunk.length);
    return chunk;
  },

  on(event: string, listener: StdinListener): StdinStream {
    if (!_stdinListeners[event]) _stdinListeners[event] = [];
    _stdinListeners[event].push(listener);

    if ((_getStdinIsTTY() || _getStreamStdin()) && (event === "data" || event === "end" || event === "close")) {
      ensureLiveStdinStarted();
    }
    if (event === "data" && this.paused) {
      this.resume();
    }

    // When 'end' listener is added and we have data, emit everything synchronously
    // This works because typical patterns register 'data' then 'end' listeners
    if (event === "end" && getStdinData() && !getStdinEnded()) {
      setStdinFlowMode(true);
      // Emit synchronously - all listeners should be registered by now
      _emitStdinData();
    }
    return this;
  },

  once(event: string, listener: StdinListener): StdinStream {
    if (!_stdinOnceListeners[event]) _stdinOnceListeners[event] = [];
    _stdinOnceListeners[event].push(listener);
    return this;
  },

  off(event: string, listener: StdinListener): StdinStream {
    if (_stdinListeners[event]) {
      const idx = _stdinListeners[event].indexOf(listener);
      if (idx !== -1) _stdinListeners[event].splice(idx, 1);
    }
    return this;
  },

  removeListener(event: string, listener: StdinListener): StdinStream {
    return this.off(event, listener);
  },

  emit(event: string, ...args: unknown[]): boolean {
    const listeners = [...(_stdinListeners[event] || []), ...(_stdinOnceListeners[event] || [])];
    _stdinOnceListeners[event] = [];
    for (const listener of listeners) {
      listener(args[0]);
    }
    return listeners.length > 0;
  },

  pause(): StdinStream {
    this.paused = true;
    setStdinFlowMode(false);
    syncLiveStdinHandle(false);
    return this;
  },

  resume(): StdinStream {
    if (_getStdinIsTTY() || _getStreamStdin()) {
      ensureLiveStdinStarted();
      syncLiveStdinHandle(true);
    }
    this.paused = false;
    setStdinFlowMode(true);
    flushLiveStdinBuffer();
    _emitStdinData();
    return this;
  },

  setEncoding(enc: string): StdinStream {
    this.encoding = enc;
    return this;
  },

  setRawMode(mode: boolean): StdinStream {
    if (!_getStdinIsTTY()) {
      throw new Error("setRawMode is not supported when stdin is not a TTY");
    }
    if (typeof _ptySetRawMode !== "undefined") {
      _ptySetRawMode.applySync(undefined, [mode]);
    }
    this.isRaw = mode;
    return this;
  },

  get isTTY(): boolean { return _getStdinIsTTY(); },

  // For readline compatibility
  [Symbol.asyncIterator]: async function* () {
    const lines = getStdinData().split("\n");
    for (const line of lines) {
      if (line) yield line;
    }
  },
};

// hrtime function with bigint method
function hrtime(prev?: [number, number]): [number, number] {
  const now = getNowMs();
  const seconds = Math.floor(now / 1000);
  const nanoseconds = Math.floor((now % 1000) * 1e6);

  if (prev) {
    let diffSec = seconds - prev[0];
    let diffNano = nanoseconds - prev[1];
    if (diffNano < 0) {
      diffSec -= 1;
      diffNano += 1e9;
    }
    return [diffSec, diffNano];
  }

  return [seconds, nanoseconds];
}

hrtime.bigint = function (): bigint {
  const now = getNowMs();
  return BigInt(Math.floor(now * 1e6));
};

// Internal state
let _cwd = config.cwd;
let _umask = 0o022;

// The process object — typed loosely as a polyfill, cast to typeof nodeProcess on export
const process: Record<string, unknown> & {
  stdout: StdioWriteStream;
  stderr: StdioWriteStream;
  stdin: StdinStream;
  pid: number;
  ppid: number;
  env: Record<string, string>;
  _cwd: string;
  _umask: number;
} = {
  // Static properties
  platform: config.platform as NodeJS.Platform,
  arch: config.arch as NodeJS.Architecture,
  version: config.version,
  versions: {
    node: config.version.replace(/^v/, ""),
    v8: "11.3.244.8",
    uv: "1.44.2",
    zlib: "1.2.13",
    brotli: "1.0.9",
    ares: "1.19.0",
    modules: "108",
    nghttp2: "1.52.0",
    napi: "8",
    llhttp: "8.1.0",
    openssl: "3.0.8",
    cldr: "42.0",
    icu: "72.1",
    tz: "2022g",
    unicode: "15.0",
  },
  pid: config.pid,
  ppid: config.ppid,
  execPath: config.execPath,
  execArgv: [],
  argv: config.argv,
  argv0: config.argv[0] || "node",
  title: "node",
  env: config.env,

  // Config stubs
  config: {
    target_defaults: {
      cflags: [],
      default_configuration: "Release",
      defines: [],
      include_dirs: [],
      libraries: [],
    },
    variables: {
      node_prefix: "/usr",
      node_shared_libuv: false,
    },
  },

  release: {
    name: "node",
    sourceUrl:
      "https://nodejs.org/download/release/v20.0.0/node-v20.0.0.tar.gz",
    headersUrl:
      "https://nodejs.org/download/release/v20.0.0/node-v20.0.0-headers.tar.gz",
  },

  // Feature flags
  features: {
    inspector: false,
    debug: false,
    uv: true,
    ipv6: true,
    tls_alpn: true,
    tls_sni: true,
    tls_ocsp: true,
    tls: true,
  },

  // Methods
  cwd(): string {
    return _cwd;
  },

  chdir(dir: string): void {
    // Validate directory exists in VFS before setting cwd
    let statJson: string;
    try {
      statJson = _fs.stat.applySyncPromise(undefined, [dir]);
    } catch {
      const err = new Error(`ENOENT: no such file or directory, chdir '${dir}'`) as Error & { code: string; errno: number; syscall: string; path: string };
      err.code = "ENOENT";
      err.errno = -2;
      err.syscall = "chdir";
      err.path = dir;
      throw err;
    }
    const parsed = JSON.parse(statJson);
    if (!parsed.isDirectory) {
      const err = new Error(`ENOTDIR: not a directory, chdir '${dir}'`) as Error & { code: string; errno: number; syscall: string; path: string };
      err.code = "ENOTDIR";
      err.errno = -20;
      err.syscall = "chdir";
      err.path = dir;
      throw err;
    }
    _cwd = dir;
  },

  get exitCode(): number | undefined {
    return _exitCode;
  },

  set exitCode(code: number | undefined) {
    _exitCode = code ?? 0;
  },

  exit(code?: number): never {
    const exitCode = code !== undefined ? code : _exitCode;
    _exitCode = exitCode;
    _exited = true;

    // Fire exit event
    try {
      _emit("exit", exitCode);
    } catch (_e) {
      // Ignore errors in exit handlers
    }

    // Throw to stop execution
    throw new ProcessExitError(exitCode);
  },

  abort(): never {
    return (process as unknown as { exit: (code: number) => never }).exit(1);
  },

  nextTick(callback: (...args: unknown[]) => void, ...args: unknown[]): void {
    _nextTickQueue.push({ callback, args });
    scheduleNextTickFlush();
  },

  hrtime: hrtime as typeof nodeProcess.hrtime,

  getuid(): number {
    return config.uid;
  },
  getgid(): number {
    return config.gid;
  },
  geteuid(): number {
    return config.uid;
  },
  getegid(): number {
    return config.gid;
  },
  getgroups(): number[] {
    return [config.gid];
  },

  setuid(): void {},
  setgid(): void {},
  seteuid(): void {},
  setegid(): void {},
  setgroups(): void {},

  umask(mask?: number): number {
    const oldMask = _umask;
    if (mask !== undefined) {
      _umask = mask;
    }
    return oldMask;
  },

  uptime(): number {
    return (getNowMs() - _processStartTime) / 1000;
  },

  memoryUsage(): NodeJS.MemoryUsage {
    return {
      rss: 50 * 1024 * 1024,
      heapTotal: 20 * 1024 * 1024,
      heapUsed: 10 * 1024 * 1024,
      external: 1 * 1024 * 1024,
      arrayBuffers: 500 * 1024,
    };
  },

  cpuUsage(prev?: NodeJS.CpuUsage): NodeJS.CpuUsage {
    const usage = {
      user: 1000000,
      system: 500000,
    };

    if (prev) {
      return {
        user: usage.user - prev.user,
        system: usage.system - prev.system,
      };
    }

    return usage;
  },

  resourceUsage(): NodeJS.ResourceUsage {
    return {
      userCPUTime: 1000000,
      systemCPUTime: 500000,
      maxRSS: 50 * 1024,
      sharedMemorySize: 0,
      unsharedDataSize: 0,
      unsharedStackSize: 0,
      minorPageFault: 0,
      majorPageFault: 0,
      swappedOut: 0,
      fsRead: 0,
      fsWrite: 0,
      ipcSent: 0,
      ipcReceived: 0,
      signalsCount: 0,
      voluntaryContextSwitches: 0,
      involuntaryContextSwitches: 0,
    };
  },

  kill(pid: number, signal?: string | number): true {
    if (pid !== process.pid) {
      const err = new Error("Operation not permitted") as NodeJS.ErrnoException;
      err.code = "EPERM";
      err.errno = -1;
      err.syscall = "kill";
      throw err;
    }
    // Resolve signal name to number (default SIGTERM)
    const sigNum = _resolveSignal(signal);
    if (sigNum === 0) {
      return true;
    }

    const sigName = _signalNamesByNumber[sigNum] ?? `SIG${sigNum}`;
    if (_emit(sigName, sigName)) {
      return true;
    }
    if (_ignoredSelfSignals.has(sigName)) {
      return true;
    }

    // Unhandled fatal self-signals exit with 128 + signal number.
    return (process as unknown as { exit: (code: number) => never }).exit(128 + sigNum);
  },

  // EventEmitter methods
  on(event: string, listener: EventListener) {
    return _addListener(event, listener);
  },

  once(event: string, listener: EventListener) {
    return _addListener(event, listener, true);
  },

  removeListener(event: string, listener: EventListener) {
    return _removeListener(event, listener);
  },

  // off is an alias for removeListener (assigned below to be same reference)
  off: null as unknown as (event: string, listener: EventListener) => unknown,

  removeAllListeners(event?: string) {
    if (event) {
      delete _processListeners[event];
      delete _processOnceListeners[event];
    } else {
      Object.keys(_processListeners).forEach((k) => delete _processListeners[k]);
      Object.keys(_processOnceListeners).forEach(
        (k) => delete _processOnceListeners[k]
      );
    }
    return process;
  },

  addListener(event: string, listener: EventListener) {
    return _addListener(event, listener);
  },

  emit(event: string, ...args: unknown[]): boolean {
    return _emit(event, ...args);
  },

  listeners(event: string): EventListener[] {
    return [
      ...(_processListeners[event] || []),
      ...(_processOnceListeners[event] || []),
    ];
  },

  listenerCount(event: string): number {
    return (
      (_processListeners[event] || []).length +
      (_processOnceListeners[event] || []).length
    );
  },

  prependListener(event: string, listener: EventListener) {
    if (!_processListeners[event]) {
      _processListeners[event] = [];
    }
    _processListeners[event].unshift(listener);
    return process;
  },

  prependOnceListener(event: string, listener: EventListener) {
    if (!_processOnceListeners[event]) {
      _processOnceListeners[event] = [];
    }
    _processOnceListeners[event].unshift(listener);
    return process;
  },

  eventNames(): (string | symbol)[] {
    return [
      ...new Set([
        ...Object.keys(_processListeners),
        ...Object.keys(_processOnceListeners),
      ]),
    ];
  },

  setMaxListeners(n: number) {
    _processMaxListeners = n;
    return process;
  },
  getMaxListeners(): number {
    return _processMaxListeners;
  },
  rawListeners(event: string): EventListener[] {
    return (process as unknown as { listeners: (event: string) => EventListener[] }).listeners(event);
  },

  // Stdio streams
  stdout: _stdout,
  stderr: _stderr,
  stdin: _stdin,

  // Process state
  connected: false,

  // Module info (will be set by createRequire)
  mainModule: undefined,

  // No-op methods for compatibility
  emitWarning(warning: string | Error): void {
    const msg = typeof warning === "string" ? warning : warning.message;
    _emit("warning", { message: msg, name: "Warning" });
  },

  binding(_name: string): never {
    throw new Error("process.binding is not supported in sandbox");
  },

  _linkedBinding(_name: string): never {
    throw new Error("process._linkedBinding is not supported in sandbox");
  },

  dlopen(): void {
    throw new Error("process.dlopen is not supported");
  },

  hasUncaughtExceptionCaptureCallback(): boolean {
    return false;
  },
  setUncaughtExceptionCaptureCallback(): void {},

  // Send for IPC (no-op)
  send(): boolean {
    return false;
  },
  disconnect(): void {},

  // Report
  report: {
    directory: "",
    filename: "",
    compact: false,
    signal: "SIGUSR2",
    reportOnFatalError: false,
    reportOnSignal: false,
    reportOnUncaughtException: false,
    getReport(): Record<string, unknown> {
      return {};
    },
    writeReport(): string {
      return "";
    },
  },

  // Debug port
  debugPort: 9229,

  // Internal state
  _cwd: config.cwd,
  _umask: 0o022,
};

function applyProcessConfig(nextConfig: ReturnType<typeof readProcessConfig>): void {
  // Reset per-execution stdin state because the process bridge bundle is reused
  // across V8 sessions. Without this, one streamed-stdin session can leave the
  // next session stuck with stdin already marked as started or ended.
  syncLiveStdinHandle(false);
  _stdinLiveBuffer = "";
  _stdinLiveStarted = false;
  _stdinLiveDecoder = new TextDecoder();
  for (const key of Object.keys(_stdinListeners)) {
    _stdinListeners[key] = [];
  }
  for (const key of Object.keys(_stdinOnceListeners)) {
    _stdinOnceListeners[key] = [];
  }
  setStdinDataValue(nextConfig.stdin ?? "");
  setStdinPosition(0);
  setStdinEnded(false);
  setStdinFlowMode(false);

  config = nextConfig;
  _cwd = nextConfig.cwd;
  process.platform = nextConfig.platform as NodeJS.Platform;
  process.arch = nextConfig.arch as NodeJS.Architecture;
  process.version = nextConfig.version;
  process.pid = nextConfig.pid;
  process.ppid = nextConfig.ppid;
  process.execPath = nextConfig.execPath;
  process.argv = nextConfig.argv;
  process.argv0 = nextConfig.argv[0] || "node";
  process.env = nextConfig.env;
  process._cwd = nextConfig.cwd;
  process.stdin.paused = true;
  process.stdin.encoding = null;
  process.stdin.isRaw = false;
  (process.versions as Record<string, string>).node = nextConfig.version.replace(/^v/, "");
}

exposeCustomGlobal("__runtimeRefreshProcessConfig", () => {
  applyProcessConfig(readProcessConfig());
});

// Make process.off === process.removeListener (same function reference)
process.off = process.removeListener;

// Add memoryUsage.rss
(process.memoryUsage as unknown as Record<string, () => number>).rss =
  function (): number {
    return 50 * 1024 * 1024;
  };

// Match Node.js Object.prototype.toString.call(process) === '[object process]'
Object.defineProperty(process, Symbol.toStringTag, {
  value: "process",
  writable: false,
  configurable: true,
  enumerable: false,
});

export default process as unknown as typeof nodeProcess;

// ============================================================================
// Global polyfills
// ============================================================================

const TIMER_DISPATCH = {
  create: "kernelTimerCreate",
  arm: "kernelTimerArm",
  clear: "kernelTimerClear",
} as const;

type TimerEntry = {
  handle: TimerHandle;
  callback: (...args: unknown[]) => void;
  args: unknown[];
  repeat: boolean;
};

type NextTickEntry = {
  callback: (...args: unknown[]) => void;
  args: unknown[];
};

// queueMicrotask fallback
const _queueMicrotask =
  typeof queueMicrotask === "function"
    ? queueMicrotask
    : function (fn: () => void): void {
        Promise.resolve().then(fn);
      };

function normalizeTimerDelay(delay: number | undefined): number {
  const numericDelay = Number(delay ?? 0);
  if (!Number.isFinite(numericDelay) || numericDelay <= 0) {
    return 0;
  }
  return Math.floor(numericDelay);
}

function getTimerId(timer: TimerHandle | number | undefined): number | undefined {
  if (timer && typeof timer === "object" && timer._id !== undefined) {
    return timer._id;
  }
  if (typeof timer === "number") {
    return timer;
  }
  return undefined;
}

function createKernelTimer(delayMs: number, repeat: boolean): number {
  try {
    return bridgeDispatchSync<number>(TIMER_DISPATCH.create, delayMs, repeat);
  } catch (error) {
    if (error instanceof Error && error.message.includes("EAGAIN")) {
      throw new Error(
        "ERR_RESOURCE_BUDGET_EXCEEDED: maximum number of timers exceeded",
      );
    }
    throw error;
  }
}

function armKernelTimer(timerId: number): void {
  bridgeDispatchSync<void>(TIMER_DISPATCH.arm, timerId);
}

/**
 * Timer handle that mimics Node.js Timeout (ref/unref/Symbol.toPrimitive).
 * Timers with delay > 0 use the host's `_scheduleTimer` bridge to sleep
 * without blocking the isolate's event loop.
 */
class TimerHandle {
  _id: number;
  _destroyed: boolean;
  constructor(id: number) {
    this._id = id;
    this._destroyed = false;
  }
  ref(): this {
    return this;
  }
  unref(): this {
    return this;
  }
  hasRef(): boolean {
    return true;
  }
  refresh(): this {
    return this;
  }
  [Symbol.toPrimitive](): number {
    return this._id;
  }
}

const _timerEntries = new Map<number, TimerEntry>();
let _timerDrainResolvers: Array<() => void> = [];

/**
 * Check if all timers have been drained and resolve any waiters.
 * Called after a timer fires or is cleared.
 */
function checkTimerDrain(): void {
  if (_timerEntries.size === 0 && _timerDrainResolvers.length > 0) {
    const resolvers = _timerDrainResolvers;
    _timerDrainResolvers = [];
    resolvers.forEach((r) => r());
  }
}

/**
 * Returns the number of pending timer entries (setTimeout + setInterval).
 * Used by _waitForActiveHandles to detect pending async work.
 */
function _getPendingTimerCount(): number {
  return _timerEntries.size;
}

/**
 * Returns a Promise that resolves when all timer entries have been drained.
 * If no timers are pending, resolves immediately.
 */
function _waitForTimerDrain(): Promise<void> {
  if (_timerEntries.size === 0) return Promise.resolve();
  return new Promise((resolve) => {
    _timerDrainResolvers.push(resolve);
  });
}

const _nextTickQueue: NextTickEntry[] = [];
let _nextTickScheduled = false;

function flushNextTickQueue(): void {
  _nextTickScheduled = false;

  while (_nextTickQueue.length > 0) {
    const entry = _nextTickQueue.shift();
    if (!entry) {
      break;
    }

    try {
      entry.callback(...entry.args);
    } catch (error) {
      const outcome = routeAsyncCallbackError(error);
      if (!outcome.handled && outcome.rethrow !== null) {
        _nextTickQueue.length = 0;
        scheduleAsyncRethrow(outcome.rethrow);
      }
      return;
    }
  }
}

function scheduleNextTickFlush(): void {
  if (_nextTickScheduled) {
    return;
  }
  _nextTickScheduled = true;
  _queueMicrotask(flushNextTickQueue);
}

function timerDispatch(_eventType: string, payload: unknown): void {
  const timerId =
    typeof payload === "number"
      ? payload
      : Number((payload as { timerId?: unknown } | null)?.timerId);
  if (!Number.isFinite(timerId)) return;

  const entry = _timerEntries.get(timerId);
  if (!entry) return;

  if (!entry.repeat) {
    entry.handle._destroyed = true;
    _timerEntries.delete(timerId);
  }

  try {
    entry.callback(...entry.args);
  } catch (error) {
    const outcome = routeAsyncCallbackError(error);
    if (!outcome.handled && outcome.rethrow !== null) {
      throw outcome.rethrow;
    }
    return;
  }

  if (entry.repeat && _timerEntries.has(timerId)) {
    armKernelTimer(timerId);
  }

  checkTimerDrain();
}

export function setTimeout(
  callback: (...args: unknown[]) => void,
  delay?: number,
  ...args: unknown[]
): TimerHandle {
  const actualDelay = normalizeTimerDelay(delay);
  const id = createKernelTimer(actualDelay, false);
  const handle = new TimerHandle(id);
  _timerEntries.set(id, {
    handle,
    callback,
    args,
    repeat: false,
  });
  armKernelTimer(id);

  return handle;
}

export function clearTimeout(timer: TimerHandle | number | undefined): void {
  const id = getTimerId(timer);
  if (id === undefined) return;
  const entry = _timerEntries.get(id);
  if (entry) {
    entry.handle._destroyed = true;
    _timerEntries.delete(id);
  }
  bridgeDispatchSync<void>(TIMER_DISPATCH.clear, id);
  checkTimerDrain();
}

export function setInterval(
  callback: (...args: unknown[]) => void,
  delay?: number,
  ...args: unknown[]
): TimerHandle {
  const actualDelay = Math.max(1, normalizeTimerDelay(delay));
  const id = createKernelTimer(actualDelay, true);
  const handle = new TimerHandle(id);
  _timerEntries.set(id, {
    handle,
    callback,
    args,
    repeat: true,
  });
  armKernelTimer(id);

  return handle;
}

export function clearInterval(timer: TimerHandle | number | undefined): void {
  clearTimeout(timer);
}

exposeCustomGlobal("_timerDispatch", timerDispatch);
exposeCustomGlobal("_getPendingTimerCount", _getPendingTimerCount);
exposeCustomGlobal("_waitForTimerDrain", _waitForTimerDrain);

export function setImmediate(
  callback: (...args: unknown[]) => void,
  ...args: unknown[]
): TimerHandle {
  return setTimeout(callback, 0, ...args);
}

export function clearImmediate(id: TimerHandle | number | undefined): void {
  clearTimeout(id);
}

// TextEncoder and TextDecoder - re-export from polyfills
export { URL, URLSearchParams };
export { TextEncoder, TextDecoder, Event, CustomEvent, EventTarget };

// Buffer - use buffer package polyfill
export const Buffer = BufferPolyfill;

function throwUnsupportedCryptoApi(api: "getRandomValues" | "randomUUID"): never {
  throw new Error(`crypto.${api} is not supported in sandbox`);
}

interface SandboxCryptoKeyData {
	type: "public" | "private" | "secret";
	extractable: boolean;
	algorithm: Record<string, unknown>;
	usages: string[];
	_pem?: string;
	_jwk?: Record<string, unknown>;
	_raw?: string;
	_sourceKeyObjectData?: Record<string, unknown>;
}

const kCryptoKeyToken = Symbol("secureExecCryptoKey");
const kCryptoToken = Symbol("secureExecCrypto");
const kSubtleToken = Symbol("secureExecSubtle");
const ERR_INVALID_THIS = "ERR_INVALID_THIS";
const ERR_ILLEGAL_CONSTRUCTOR = "ERR_ILLEGAL_CONSTRUCTOR";

function createNodeTypeError(message: string, code: string): TypeError & { code: string } {
	const error = new TypeError(message) as TypeError & { code: string };
	error.code = code;
	return error;
}

function createDomLikeError(name: string, code: number, message: string): Error & { code: number } {
	const error = new Error(message) as Error & { code: number };
	error.name = name;
	error.code = code;
	return error;
}

function assertCryptoReceiver(receiver: unknown): asserts receiver is SandboxCrypto {
	if (!(receiver instanceof SandboxCrypto) || (receiver as SandboxCrypto)._token !== kCryptoToken) {
		throw createNodeTypeError("Value of \"this\" must be of type Crypto", ERR_INVALID_THIS);
	}
}

function assertSubtleReceiver(receiver: unknown): asserts receiver is SandboxSubtleCrypto {
	if (
		!(receiver instanceof SandboxSubtleCrypto) ||
		(receiver as SandboxSubtleCrypto)._token !== kSubtleToken
	) {
		throw createNodeTypeError("Value of \"this\" must be of type SubtleCrypto", ERR_INVALID_THIS);
	}
}

function isIntegerTypedArray(value: unknown): value is ArrayBufferView {
	if (!ArrayBuffer.isView(value) || value instanceof DataView) {
		return false;
	}

	return (
		value instanceof Int8Array ||
		value instanceof Int16Array ||
		value instanceof Int32Array ||
		value instanceof Uint8Array ||
		value instanceof Uint16Array ||
		value instanceof Uint32Array ||
		value instanceof Uint8ClampedArray ||
		value instanceof BigInt64Array ||
		value instanceof BigUint64Array ||
		BufferPolyfill.isBuffer(value)
	);
}

function toBase64(data: BufferSource | string): string {
	if (typeof data === "string") {
		return BufferPolyfill.from(data).toString("base64");
	}

	if (data instanceof ArrayBuffer) {
		return BufferPolyfill.from(new Uint8Array(data)).toString("base64");
	}

	if (ArrayBuffer.isView(data)) {
		return BufferPolyfill.from(
			new Uint8Array(data.buffer, data.byteOffset, data.byteLength),
		).toString("base64");
	}

	return BufferPolyfill.from(data).toString("base64");
}

function toArrayBuffer(data: string): ArrayBuffer {
	const buf = BufferPolyfill.from(data, "base64");
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
}

function normalizeAlgorithm(algorithm: unknown): Record<string, unknown> {
	if (typeof algorithm === "string") {
		return { name: algorithm };
	}

	return (algorithm ?? {}) as Record<string, unknown>;
}

function normalizeBridgeAlgorithm(algorithm: unknown): Record<string, unknown> {
	const normalized = { ...normalizeAlgorithm(algorithm) };
	const hash = normalized.hash;
	const publicExponent = normalized.publicExponent;
	const iv = normalized.iv;
	const additionalData = normalized.additionalData;
	const salt = normalized.salt;
	const info = normalized.info;
	const context = normalized.context;
	const label = normalized.label;
	const publicKey = normalized.public;

	if (hash) {
		normalized.hash = normalizeAlgorithm(hash);
	}
	if (publicExponent && ArrayBuffer.isView(publicExponent)) {
		normalized.publicExponent = BufferPolyfill.from(
			new Uint8Array(
				publicExponent.buffer,
				publicExponent.byteOffset,
				publicExponent.byteLength,
			),
		).toString("base64");
	}
	if (iv) {
		normalized.iv = toBase64(iv as BufferSource);
	}
	if (additionalData) {
		normalized.additionalData = toBase64(additionalData as BufferSource);
	}
	if (salt) {
		normalized.salt = toBase64(salt as BufferSource);
	}
	if (info) {
		normalized.info = toBase64(info as BufferSource);
	}
	if (context) {
		normalized.context = toBase64(context as BufferSource);
	}
	if (label) {
		normalized.label = toBase64(label as BufferSource);
	}
	if (
		publicKey &&
		typeof publicKey === "object" &&
		"_keyData" in (publicKey as Record<string, unknown>)
	) {
		normalized.public = (publicKey as SandboxCryptoKey)._keyData;
	}

	return normalized;
}

class SandboxCryptoKey {
	readonly type: "public" | "private" | "secret";
	readonly extractable: boolean;
	readonly algorithm: Record<string, unknown>;
	readonly usages: string[];
	readonly _keyData: SandboxCryptoKeyData;
	readonly _pem?: string;
	readonly _jwk?: Record<string, unknown>;
	readonly _raw?: string;
	readonly _sourceKeyObjectData?: Record<string, unknown>;
	readonly [kCryptoKeyToken]: true;

	constructor(keyData?: SandboxCryptoKeyData, token?: symbol) {
		if (token !== kCryptoKeyToken || !keyData) {
			throw createNodeTypeError("Illegal constructor", ERR_ILLEGAL_CONSTRUCTOR);
		}

		this.type = keyData.type;
		this.extractable = keyData.extractable;
		this.algorithm = keyData.algorithm;
		this.usages = keyData.usages;
		this._keyData = keyData;
		this._pem = keyData._pem;
		this._jwk = keyData._jwk;
		this._raw = keyData._raw;
		this._sourceKeyObjectData = keyData._sourceKeyObjectData;
		this[kCryptoKeyToken] = true;
	}
}

Object.defineProperty(SandboxCryptoKey.prototype, Symbol.toStringTag, {
	value: "CryptoKey",
	configurable: true,
});

Object.defineProperty(SandboxCryptoKey, Symbol.hasInstance, {
	value(candidate: unknown) {
		return Boolean(
			candidate &&
				typeof candidate === "object" &&
				(
					(candidate as { [kCryptoKeyToken]?: boolean })[kCryptoKeyToken] === true ||
					(
						"_keyData" in (candidate as Record<string, unknown>) &&
						(candidate as { [Symbol.toStringTag]?: string })[Symbol.toStringTag] === "CryptoKey"
					)
				),
		);
	},
	configurable: true,
});

function createCryptoKey(keyData: SandboxCryptoKeyData): SandboxCryptoKey {
	const globalCryptoKey = globalThis.CryptoKey as
		| ({ prototype?: object } & (new (...args: any[]) => CryptoKey))
		| undefined;
	if (
		typeof globalCryptoKey === "function" &&
		globalCryptoKey.prototype &&
		globalCryptoKey.prototype !== SandboxCryptoKey.prototype
	) {
		const key = Object.create(globalCryptoKey.prototype) as SandboxCryptoKey & {
			type: SandboxCryptoKey["type"];
			extractable: SandboxCryptoKey["extractable"];
			algorithm: SandboxCryptoKey["algorithm"];
			usages: SandboxCryptoKey["usages"];
			_keyData: SandboxCryptoKey["_keyData"];
			_pem: SandboxCryptoKey["_pem"];
			_jwk: SandboxCryptoKey["_jwk"];
			_raw: SandboxCryptoKey["_raw"];
			_sourceKeyObjectData: SandboxCryptoKey["_sourceKeyObjectData"];
		};
		key.type = keyData.type;
		key.extractable = keyData.extractable;
		key.algorithm = keyData.algorithm;
		key.usages = keyData.usages;
		key._keyData = keyData;
		key._pem = keyData._pem;
		key._jwk = keyData._jwk;
		key._raw = keyData._raw;
		key._sourceKeyObjectData = keyData._sourceKeyObjectData;
		return key;
	}
	return new SandboxCryptoKey(keyData, kCryptoKeyToken);
}

function subtleCall(request: Record<string, unknown>): string {
	if (typeof _cryptoSubtle === "undefined") {
		throw new Error("crypto.subtle is not supported in sandbox");
	}

	return _cryptoSubtle.applySync(undefined, [JSON.stringify(request)]);
}

class SandboxSubtleCrypto {
	readonly _token: symbol;

	constructor(token?: symbol) {
		if (token !== kSubtleToken) {
			throw createNodeTypeError("Illegal constructor", ERR_ILLEGAL_CONSTRUCTOR);
		}

		this._token = token;
	}

	digest(algorithm: unknown, data: BufferSource): Promise<ArrayBuffer> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "digest",
					algorithm: normalizeAlgorithm(algorithm).name,
					data: toBase64(data),
				}),
			) as { data: string };
			return toArrayBuffer(result.data);
		});
	}

	generateKey(
		algorithm: unknown,
		extractable: boolean,
		keyUsages: Iterable<string>,
	): Promise<SandboxCryptoKey | { publicKey: SandboxCryptoKey; privateKey: SandboxCryptoKey }> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "generateKey",
					algorithm: normalizeBridgeAlgorithm(algorithm),
					extractable,
					usages: Array.from(keyUsages),
				}),
			) as
				| { key: SandboxCryptoKeyData }
				| { publicKey: SandboxCryptoKeyData; privateKey: SandboxCryptoKeyData };
			if ("publicKey" in result && "privateKey" in result) {
				return {
					publicKey: createCryptoKey(result.publicKey),
					privateKey: createCryptoKey(result.privateKey),
				};
			}
			return createCryptoKey(result.key);
		});
	}

	importKey(
		format: string,
		keyData: BufferSource | JsonWebKey,
		algorithm: unknown,
		extractable: boolean,
		keyUsages: Iterable<string>,
	): Promise<SandboxCryptoKey> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "importKey",
					format,
					keyData: format === "jwk" ? keyData : toBase64(keyData as BufferSource),
					algorithm: normalizeBridgeAlgorithm(algorithm),
					extractable,
					usages: Array.from(keyUsages),
				}),
			) as { key: SandboxCryptoKeyData };
			return createCryptoKey(result.key);
		});
	}

	exportKey(format: string, key: SandboxCryptoKey): Promise<ArrayBuffer | JsonWebKey> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "exportKey",
					format,
					key: key._keyData,
				}),
			) as { data?: string; jwk?: JsonWebKey };
			if (format === "jwk") {
				return result.jwk as JsonWebKey;
			}
			return toArrayBuffer(result.data ?? "");
		});
	}

	encrypt(algorithm: unknown, key: SandboxCryptoKey, data: BufferSource): Promise<ArrayBuffer> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "encrypt",
					algorithm: normalizeBridgeAlgorithm(algorithm),
					key: key._keyData,
					data: toBase64(data),
				}),
			) as { data: string };
			return toArrayBuffer(result.data);
		});
	}

	decrypt(algorithm: unknown, key: SandboxCryptoKey, data: BufferSource): Promise<ArrayBuffer> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "decrypt",
					algorithm: normalizeBridgeAlgorithm(algorithm),
					key: key._keyData,
					data: toBase64(data),
				}),
			) as { data: string };
			return toArrayBuffer(result.data);
		});
	}

	sign(algorithm: unknown, key: SandboxCryptoKey, data: BufferSource): Promise<ArrayBuffer> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "sign",
					algorithm: normalizeBridgeAlgorithm(algorithm),
					key: key._keyData,
					data: toBase64(data),
				}),
			) as { data: string };
			return toArrayBuffer(result.data);
		});
	}

	verify(
		algorithm: unknown,
		key: SandboxCryptoKey,
		signature: BufferSource,
		data: BufferSource,
	): Promise<boolean> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "verify",
					algorithm: normalizeBridgeAlgorithm(algorithm),
					key: key._keyData,
					signature: toBase64(signature),
					data: toBase64(data),
				}),
			) as { result: boolean };
			return result.result;
		});
	}

	deriveBits(algorithm: unknown, baseKey: SandboxCryptoKey, length: number): Promise<ArrayBuffer> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "deriveBits",
					algorithm: normalizeBridgeAlgorithm(algorithm),
					baseKey: baseKey._keyData,
					length,
				}),
			) as { data: string };
			return toArrayBuffer(result.data);
		});
	}

	deriveKey(
		algorithm: unknown,
		baseKey: SandboxCryptoKey,
		derivedKeyAlgorithm: unknown,
		extractable: boolean,
		keyUsages: Iterable<string>,
	): Promise<SandboxCryptoKey> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "deriveKey",
					algorithm: normalizeBridgeAlgorithm(algorithm),
					baseKey: baseKey._keyData,
					derivedKeyAlgorithm: normalizeBridgeAlgorithm(derivedKeyAlgorithm),
					extractable,
					usages: Array.from(keyUsages),
				}),
			) as { key: SandboxCryptoKeyData };
			return createCryptoKey(result.key);
		});
	}

	wrapKey(
		format: string,
		key: SandboxCryptoKey,
		wrappingKey: SandboxCryptoKey,
		wrapAlgorithm: unknown,
	): Promise<ArrayBuffer> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "wrapKey",
					format,
					key: key._keyData,
					wrappingKey: wrappingKey._keyData,
					wrapAlgorithm: normalizeBridgeAlgorithm(wrapAlgorithm),
				}),
			) as { data: string };
			return toArrayBuffer(result.data);
		});
	}

	unwrapKey(
		format: string,
		wrappedKey: BufferSource,
		unwrappingKey: SandboxCryptoKey,
		unwrapAlgorithm: unknown,
		unwrappedKeyAlgorithm: unknown,
		extractable: boolean,
		keyUsages: Iterable<string>,
	): Promise<SandboxCryptoKey> {
		assertSubtleReceiver(this);

		return Promise.resolve().then(() => {
			const result = JSON.parse(
				subtleCall({
					op: "unwrapKey",
					format,
					wrappedKey: toBase64(wrappedKey),
					unwrappingKey: unwrappingKey._keyData,
					unwrapAlgorithm: normalizeBridgeAlgorithm(unwrapAlgorithm),
					unwrappedKeyAlgorithm: normalizeBridgeAlgorithm(unwrappedKeyAlgorithm),
					extractable,
					usages: Array.from(keyUsages),
				}),
			) as { key: SandboxCryptoKeyData };
			return createCryptoKey(result.key);
		});
	}
}

const subtleCrypto = new SandboxSubtleCrypto(kSubtleToken);

class SandboxCrypto {
	readonly _token: symbol;

	constructor(token?: symbol) {
		if (token !== kCryptoToken) {
			throw createNodeTypeError("Illegal constructor", ERR_ILLEGAL_CONSTRUCTOR);
		}

		this._token = token;
	}

	get subtle(): SandboxSubtleCrypto {
		assertCryptoReceiver(this);
		return subtleCrypto;
	}

	getRandomValues<T extends ArrayBufferView>(array: T): T {
		assertCryptoReceiver(this);

		if (!isIntegerTypedArray(array)) {
			throw createDomLikeError(
				"TypeMismatchError",
				17,
				"The data argument must be an integer-type TypedArray",
			);
		}

		if (typeof _cryptoRandomFill === "undefined") {
			throwUnsupportedCryptoApi("getRandomValues");
		}
		if (array.byteLength > 65536) {
			throw createDomLikeError(
				"QuotaExceededError",
				22,
				`The ArrayBufferView's byte length (${array.byteLength}) exceeds the number of bytes of entropy available via this API (65536)`,
			);
		}

		const bytes = new Uint8Array(array.buffer, array.byteOffset, array.byteLength);
		try {
			const base64 = _cryptoRandomFill.applySync(undefined, [bytes.byteLength]);
			const hostBytes = BufferPolyfill.from(base64, "base64");
			if (hostBytes.byteLength !== bytes.byteLength) {
				throw new Error("invalid host entropy size");
			}
			bytes.set(hostBytes);
			return array;
		} catch {
			throwUnsupportedCryptoApi("getRandomValues");
		}
	}

	randomUUID(): string {
		assertCryptoReceiver(this);

		if (typeof _cryptoRandomUUID === "undefined") {
			throwUnsupportedCryptoApi("randomUUID");
		}
		try {
			const uuid = _cryptoRandomUUID.applySync(undefined, []);
			if (typeof uuid !== "string") {
				throw new Error("invalid host uuid");
			}
			return uuid;
		} catch {
			throwUnsupportedCryptoApi("randomUUID");
		}
	}
}

const cryptoPolyfillInstance = new SandboxCrypto(kCryptoToken);

/**
 * Crypto polyfill that delegates to the host for entropy. `getRandomValues`
 * calls the host's `_cryptoRandomFill` bridge to get cryptographically secure
 * random bytes. Subtle crypto operations route through the host WebCrypto bridge.
 */
export const cryptoPolyfill = cryptoPolyfillInstance;

/**
 * Install all process/timer/URL/Buffer/crypto polyfills onto `globalThis`.
 * Called once during bridge initialization before user code runs.
 */
export function setupGlobals(): void {
  const g = globalThis as Record<string, unknown>;

  // Process - simple assignment is sufficient since we use external: ["process"]
  // in polyfills.ts, which prevents node-stdlib-browser's process shim from being
  // bundled and overwriting our process object.
  g.process = process;

  // Timers
  g.setTimeout = setTimeout;
  g.clearTimeout = clearTimeout;
  g.setInterval = setInterval;
  g.clearInterval = clearInterval;
  g.setImmediate = setImmediate;
  g.clearImmediate = clearImmediate;

  // queueMicrotask
  if (typeof g.queueMicrotask === "undefined") {
    g.queueMicrotask = _queueMicrotask;
  }

  // URL globals must override bootstrap fallbacks and stay non-enumerable.
  installWhatwgUrlGlobals(g as typeof globalThis);

  // WHATWG encoding and events
  g.TextEncoder = TextEncoder;
  g.TextDecoder = TextDecoder;
  g.Event = Event;
  g.CustomEvent = CustomEvent;
  g.EventTarget = EventTarget;

  // Buffer
  if (typeof g.Buffer === "undefined") {
    g.Buffer = Buffer;
  }
  const globalBuffer = g.Buffer as Record<string, unknown>;
  if (typeof globalBuffer.kMaxLength !== "number") {
    globalBuffer.kMaxLength = BUFFER_MAX_LENGTH;
  }
  if (typeof globalBuffer.kStringMaxLength !== "number") {
    globalBuffer.kStringMaxLength = BUFFER_MAX_STRING_LENGTH;
  }
  if (
    typeof globalBuffer.constants !== "object" ||
    globalBuffer.constants === null
  ) {
    globalBuffer.constants = BUFFER_CONSTANTS;
  }

  // Crypto
  if (typeof g.Crypto === "undefined") {
    g.Crypto = SandboxCrypto;
  }
  if (typeof g.SubtleCrypto === "undefined") {
    g.SubtleCrypto = SandboxSubtleCrypto;
  }
  if (typeof g.CryptoKey === "undefined") {
    g.CryptoKey = SandboxCryptoKey;
  }

  if (typeof g.crypto === "undefined") {
    g.crypto = cryptoPolyfill;
  } else {
    const cryptoObj = g.crypto as Record<string, unknown>;
    if (typeof cryptoObj.getRandomValues === "undefined") {
      cryptoObj.getRandomValues = cryptoPolyfill.getRandomValues;
    }
    if (typeof cryptoObj.randomUUID === "undefined") {
      cryptoObj.randomUUID = cryptoPolyfill.randomUUID;
    }
    if (typeof cryptoObj.subtle === "undefined") {
      cryptoObj.subtle = cryptoPolyfill.subtle;
    }
  }
}
