// child_process module polyfill for the sandbox
// Provides Node.js child_process module emulation that bridges to host
//
// Uses the active handles mechanism to keep the sandbox alive while child
// processes are running. See: docs-internal/node/ACTIVE_HANDLES.md

import type * as nodeChildProcess from "child_process";
import { exposeCustomGlobal } from "@secure-exec/core/internal/shared/global-exposure";
import type {
	ChildProcessKillBridgeRef,
	ChildProcessSpawnStartBridgeRef,
	ChildProcessSpawnSyncBridgeRef,
	ChildProcessStdinCloseBridgeRef,
	ChildProcessStdinWriteBridgeRef,
	RegisterHandleBridgeFn,
	UnregisterHandleBridgeFn,
} from "../bridge-contract.js";

// Host bridge declarations for streaming mode
declare const _childProcessSpawnStart:
  | ChildProcessSpawnStartBridgeRef
  | undefined;

declare const _childProcessStdinWrite:
  | ChildProcessStdinWriteBridgeRef
  | undefined;

declare const _childProcessStdinClose:
  | ChildProcessStdinCloseBridgeRef
  | undefined;

declare const _childProcessKill:
  | ChildProcessKillBridgeRef
  | undefined;

// Synchronous spawn - blocks until process exits, returns all output as JSON
declare const _childProcessSpawnSync:
  | ChildProcessSpawnSyncBridgeRef
  | undefined;

// Active handles functions (installed by active-handles.ts)
// See: docs-internal/node/ACTIVE_HANDLES.md
declare const _registerHandle: RegisterHandleBridgeFn;
declare const _unregisterHandle: UnregisterHandleBridgeFn;

// Child process instances — routes stream events from host to ChildProcess objects.
// Process state (running/exited) is tracked by the kernel process table; this Map
// is only for dispatching stdout/stderr/exit events to the sandbox-side objects.
const childProcessInstances = new Map<number, ChildProcess>();

/**
 * Global dispatcher invoked by the host when child process data arrives.
 * Routes stdout/stderr chunks and exit codes to the corresponding ChildProcess
 * instance by session ID, and unregisters the active handle on exit.
 */
function routeChildProcessEvent(
  sessionId: number,
  type: "stdout" | "stderr" | "exit",
  data: Uint8Array | number,
): void {
  const child = childProcessInstances.get(sessionId);
  if (!child) return;

  if (type === "stdout") {
    const buf =
      typeof Buffer !== "undefined" ? Buffer.from(data as Uint8Array) : data;
    child.stdout.emit("data", buf);
  } else if (type === "stderr") {
    const buf =
      typeof Buffer !== "undefined" ? Buffer.from(data as Uint8Array) : data;
    child.stderr.emit("data", buf);
  } else if (type === "exit") {
    child.exitCode = data as number;
    child.stdout.emit("end");
    child.stderr.emit("end");
    child.emit("close", data, null);
    child.emit("exit", data, null);
    childProcessInstances.delete(sessionId);
    // Unregister handle - allows sandbox to exit if no other handles remain
    // See: docs-internal/node/ACTIVE_HANDLES.md
    if (typeof _unregisterHandle === "function") {
      _unregisterHandle(`child:${sessionId}`);
    }
  }
}

const childProcessDispatch = (
  eventTypeOrSessionId: string | number,
  payloadOrType: unknown,
  data?: Uint8Array | number
): void => {
  if (typeof eventTypeOrSessionId === "number") {
    routeChildProcessEvent(
      eventTypeOrSessionId,
      payloadOrType as "stdout" | "stderr" | "exit",
      data as Uint8Array | number,
    );
    return;
  }

  const payload = (() => {
    if (payloadOrType && typeof payloadOrType === "object") {
      return payloadOrType as {
        sessionId?: unknown;
        dataBase64?: unknown;
        data?: unknown;
        code?: unknown;
      };
    }
    if (typeof payloadOrType === "string") {
      try {
        return JSON.parse(payloadOrType) as {
          sessionId?: unknown;
          dataBase64?: unknown;
          data?: unknown;
          code?: unknown;
        };
      } catch {
        return null;
      }
    }
    return null;
  })();
  const sessionId = typeof payload?.sessionId === "number"
    ? payload.sessionId
    : Number(payload?.sessionId);
  if (!Number.isFinite(sessionId)) {
    return;
  }

  if (eventTypeOrSessionId === "child_stdout" || eventTypeOrSessionId === "child_stderr") {
    const encoded =
      typeof payload?.dataBase64 === "string"
        ? payload.dataBase64
        : typeof payload?.data === "string"
          ? payload.data
          : "";
    const bytes =
      typeof Buffer !== "undefined"
        ? Buffer.from(encoded, "base64")
        : new Uint8Array(
            atob(encoded)
              .split("")
              .map((char) => char.charCodeAt(0)),
          );
    routeChildProcessEvent(
      sessionId,
      eventTypeOrSessionId === "child_stdout" ? "stdout" : "stderr",
      bytes,
    );
    return;
  }

  if (eventTypeOrSessionId === "child_exit") {
    const code = typeof payload?.code === "number"
      ? payload.code
      : Number(payload?.code ?? 1);
    routeChildProcessEvent(sessionId, "exit", code);
  }
};
exposeCustomGlobal("_childProcessDispatch", childProcessDispatch);

// Event listener types
type EventListener = (...args: unknown[]) => void;

// Stream stub for stdin
interface StdinStream {
  writable: boolean;
  write(data: unknown): boolean;
  end(): void;
  on(): StdinStream;
  once(): StdinStream;
  emit(): boolean;
}

// Stream stub for stdout/stderr
interface OutputStreamStub {
  readable: boolean;
  isTTY?: boolean;
  _listeners: Record<string, EventListener[]>;
  _onceListeners: Record<string, EventListener[]>;
  _bufferedChunks: unknown[];
  _ended: boolean;
  _flushScheduled: boolean;
  _maxListeners: number;
  _maxListenersWarned: Set<string>;
  on(event: string, listener: EventListener): OutputStreamStub;
  once(event: string, listener: EventListener): OutputStreamStub;
  off(event: string, listener: EventListener): OutputStreamStub;
  removeListener(event: string, listener: EventListener): OutputStreamStub;
  emit(event: string, ...args: unknown[]): boolean;
  read(): null;
  setEncoding(): OutputStreamStub;
  setMaxListeners(n: number): OutputStreamStub;
  getMaxListeners(): number;
  pipe<T extends NodeJS.WritableStream>(dest: T): T;
  pause(): OutputStreamStub;
  resume(): OutputStreamStub;
}

function hasOutputListeners(stream: OutputStreamStub, event: string): boolean {
  return (
    (stream._listeners[event]?.length ?? 0) > 0 ||
    (stream._onceListeners[event]?.length ?? 0) > 0
  );
}

function scheduleOutputFlush(stream: OutputStreamStub): void {
  if (stream._flushScheduled) {
    return;
  }
  stream._flushScheduled = true;
  queueMicrotask(() => {
    stream._flushScheduled = false;

    if (stream._bufferedChunks.length > 0 && hasOutputListeners(stream, "data")) {
      const chunks = stream._bufferedChunks.splice(0, stream._bufferedChunks.length);
      for (const chunk of chunks) {
        stream.emit("data", chunk);
      }
    }

    if (stream._ended && stream._bufferedChunks.length === 0 && hasOutputListeners(stream, "end")) {
      stream.emit("end");
    }
  });
}

/** Warn when listener count exceeds max (Node.js: warn, don't crash) */
function checkStreamMaxListeners(stream: OutputStreamStub, event: string): void {
  if (stream._maxListeners > 0 && !stream._maxListenersWarned.has(event)) {
    const total = (stream._listeners[event]?.length ?? 0) + (stream._onceListeners[event]?.length ?? 0);
    if (total > stream._maxListeners) {
      stream._maxListenersWarned.add(event);
      const warning = `MaxListenersExceededWarning: Possible EventEmitter memory leak detected. ${total} ${event} listeners added. MaxListeners is ${stream._maxListeners}. Use emitter.setMaxListeners() to increase limit`;
      if (typeof console !== "undefined" && console.error) {
        console.error(warning);
      }
    }
  }
}

// Monotonic counter for unique ChildProcess PIDs
let _nextChildPid = 1000;

/**
 * Polyfill of Node.js `ChildProcess`. Provides event-emitting stdin/stdout/stderr
 * streams. In streaming mode, data arrives via the `_childProcessDispatch` global
 * that the host calls with stdout/stderr/exit events keyed by session ID.
 */
class ChildProcess {
  private _listeners: Record<string, EventListener[]> = {};
  private _onceListeners: Record<string, EventListener[]> = {};
  private _maxListeners = 10;
  private _maxListenersWarned = new Set<string>();

  pid: number = _nextChildPid++;
  killed = false;
  exitCode: number | null = null;
  signalCode: NodeJS.Signals | null = null;
  connected = false;
  spawnfile = "";
  spawnargs: string[] = [];

  stdin: StdinStream;
  stdout: OutputStreamStub;
  stderr: OutputStreamStub;
  stdio: [StdinStream, OutputStreamStub, OutputStreamStub];

  constructor() {
    // Create stdin stream stub
    this.stdin = {
      writable: true,
      write(_data: unknown): boolean {
        return true;
      },
      end(): void {
        this.writable = false;
      },
      on(): StdinStream {
        return this;
      },
      once(): StdinStream {
        return this;
      },
      emit(): boolean {
        return false;
      },
    };

    // Create stdout stream stub
    this.stdout = {
      readable: true,
      isTTY: false,
      _listeners: {},
      _onceListeners: {},
      _bufferedChunks: [],
      _ended: false,
      _flushScheduled: false,
      _maxListeners: 10,
      _maxListenersWarned: new Set(),
      on(event: string, listener: EventListener): OutputStreamStub {
        if (!this._listeners[event]) this._listeners[event] = [];
        this._listeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      once(event: string, listener: EventListener): OutputStreamStub {
        if (!this._onceListeners[event]) this._onceListeners[event] = [];
        this._onceListeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      off(event: string, listener: EventListener): OutputStreamStub {
        if (this._listeners[event]) {
          const idx = this._listeners[event].indexOf(listener);
          if (idx !== -1) this._listeners[event].splice(idx, 1);
        }
        if (this._onceListeners[event]) {
          const idx = this._onceListeners[event].indexOf(listener);
          if (idx !== -1) this._onceListeners[event].splice(idx, 1);
        }
        return this;
      },
      removeListener(event: string, listener: EventListener): OutputStreamStub {
        return this.off(event, listener);
      },
      emit(event: string, ...args: unknown[]): boolean {
        if (event === "data" && !hasOutputListeners(this, "data")) {
          this._bufferedChunks.push(args[0]);
          return false;
        }
        if (event === "end") {
          this._ended = true;
          if (!hasOutputListeners(this, "end")) {
            return false;
          }
        }
        if (this._listeners[event]) {
          this._listeners[event].forEach((fn) => fn(...args));
        }
        if (this._onceListeners[event]) {
          this._onceListeners[event].forEach((fn) => fn(...args));
          this._onceListeners[event] = [];
        }
        return true;
      },
      read(): null {
        return null;
      },
      setEncoding(): OutputStreamStub {
        return this;
      },
      setMaxListeners(n: number): OutputStreamStub {
        this._maxListeners = n;
        return this;
      },
      getMaxListeners(): number {
        return this._maxListeners;
      },
      pipe<T extends NodeJS.WritableStream>(dest: T): T {
        return dest;
      },
      pause(): OutputStreamStub {
        return this;
      },
      resume(): OutputStreamStub {
        return this;
      },
    };

    // Create stderr stream stub
    this.stderr = {
      readable: true,
      isTTY: false,
      _listeners: {},
      _onceListeners: {},
      _bufferedChunks: [],
      _ended: false,
      _flushScheduled: false,
      _maxListeners: 10,
      _maxListenersWarned: new Set(),
      on(event: string, listener: EventListener): OutputStreamStub {
        if (!this._listeners[event]) this._listeners[event] = [];
        this._listeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      once(event: string, listener: EventListener): OutputStreamStub {
        if (!this._onceListeners[event]) this._onceListeners[event] = [];
        this._onceListeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      off(event: string, listener: EventListener): OutputStreamStub {
        if (this._listeners[event]) {
          const idx = this._listeners[event].indexOf(listener);
          if (idx !== -1) this._listeners[event].splice(idx, 1);
        }
        if (this._onceListeners[event]) {
          const idx = this._onceListeners[event].indexOf(listener);
          if (idx !== -1) this._onceListeners[event].splice(idx, 1);
        }
        return this;
      },
      removeListener(event: string, listener: EventListener): OutputStreamStub {
        return this.off(event, listener);
      },
      emit(event: string, ...args: unknown[]): boolean {
        if (event === "data" && !hasOutputListeners(this, "data")) {
          this._bufferedChunks.push(args[0]);
          return false;
        }
        if (event === "end") {
          this._ended = true;
          if (!hasOutputListeners(this, "end")) {
            return false;
          }
        }
        if (this._listeners[event]) {
          this._listeners[event].forEach((fn) => fn(...args));
        }
        if (this._onceListeners[event]) {
          this._onceListeners[event].forEach((fn) => fn(...args));
          this._onceListeners[event] = [];
        }
        return true;
      },
      read(): null {
        return null;
      },
      setEncoding(): OutputStreamStub {
        return this;
      },
      setMaxListeners(n: number): OutputStreamStub {
        this._maxListeners = n;
        return this;
      },
      getMaxListeners(): number {
        return this._maxListeners;
      },
      pipe<T extends NodeJS.WritableStream>(dest: T): T {
        return dest;
      },
      pause(): OutputStreamStub {
        return this;
      },
      resume(): OutputStreamStub {
        return this;
      },
    };

    this.stdio = [this.stdin, this.stdout, this.stderr];
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    this._checkMaxListeners(event);
    return this;
  }

  once(event: string, listener: EventListener): this {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    this._checkMaxListeners(event);
    return this;
  }

  off(event: string, listener: EventListener): this {
    if (this._listeners[event]) {
      const idx = this._listeners[event].indexOf(listener);
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  setMaxListeners(n: number): this {
    this._maxListeners = n;
    return this;
  }

  getMaxListeners(): number {
    return this._maxListeners;
  }

  private _checkMaxListeners(event: string): void {
    if (this._maxListeners > 0 && !this._maxListenersWarned.has(event)) {
      const total = (this._listeners[event]?.length ?? 0) + (this._onceListeners[event]?.length ?? 0);
      if (total > this._maxListeners) {
        this._maxListenersWarned.add(event);
        const warning = `MaxListenersExceededWarning: Possible EventEmitter memory leak detected. ${total} ${event} listeners added to [ChildProcess]. MaxListeners is ${this._maxListeners}. Use emitter.setMaxListeners() to increase limit`;
        if (typeof console !== "undefined" && console.error) {
          console.error(warning);
        }
      }
    }
  }

  emit(event: string, ...args: unknown[]): boolean {
    let handled = false;
    if (this._listeners[event]) {
      this._listeners[event].forEach((fn) => {
        fn(...args);
        handled = true;
      });
    }
    if (this._onceListeners[event]) {
      this._onceListeners[event].forEach((fn) => {
        fn(...args);
        handled = true;
      });
      this._onceListeners[event] = [];
    }
    return handled;
  }

  kill(_signal?: NodeJS.Signals | number): boolean {
    this.killed = true;
    this.signalCode = (typeof _signal === "string" ? _signal : "SIGTERM") as NodeJS.Signals;
    return true;
  }

  ref(): this {
    return this;
  }

  unref(): this {
    return this;
  }

  disconnect(): void {
    this.connected = false;
  }

  _complete(stdout: string, stderr: string, code: number): void {
    this.exitCode = code;

    // Emit data events for stdout/stderr as single chunks
    if (stdout) {
      const buf = typeof Buffer !== "undefined" ? Buffer.from(stdout) : stdout;
      this.stdout.emit("data", buf);
    }
    if (stderr) {
      const buf = typeof Buffer !== "undefined" ? Buffer.from(stderr) : stderr;
      this.stderr.emit("data", buf);
    }

    // Emit end events
    this.stdout.emit("end");
    this.stderr.emit("end");

    // Emit close event (code, signal)
    this.emit("close", code, this.signalCode);

    // Emit exit event
    this.emit("exit", code, this.signalCode);
  }
}

// ExecError type
interface ExecError extends Error {
  code?: number;
  killed?: boolean;
  signal?: string | null;
  cmd?: string;
  stdout?: string;
  stderr?: string;
  status?: number;
  output?: [null, string, string];
}

// exec - execute shell command, callback when done
// Uses spawn("sh", ["-c", command]) internally
function exec(
  command: string,
  options?: nodeChildProcess.ExecOptions | ((error: ExecError | null, stdout: string, stderr: string) => void),
  callback?: (error: ExecError | null, stdout: string, stderr: string) => void
): ChildProcess {
  if (typeof options === "function") {
    callback = options;
    options = {};
  }

  // Use spawn with shell to execute the command
  const child = spawn("bash", ["-c", command], { shell: false });
  child.spawnargs = ["bash", "-c", command];
  child.spawnfile = "bash";

  // Collect output and invoke callback with maxBuffer enforcement
  const maxBuffer = (options as nodeChildProcess.ExecOptions | undefined)?.maxBuffer ?? 1024 * 1024;
  let stdout = "";
  let stderr = "";
  let stdoutBytes = 0;
  let stderrBytes = 0;
  let maxBufferExceeded = false;

  child.stdout.on("data", (data: unknown) => {
    if (maxBufferExceeded) return;
    const chunk = String(data);
    stdout += chunk;
    stdoutBytes += chunk.length;
    if (stdoutBytes > maxBuffer) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });

  child.stderr.on("data", (data: unknown) => {
    if (maxBufferExceeded) return;
    const chunk = String(data);
    stderr += chunk;
    stderrBytes += chunk.length;
    if (stderrBytes > maxBuffer) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });

  child.on("close", (...args: unknown[]) => {
    const code = args[0] as number;
    if (callback) {
      if (maxBufferExceeded) {
        const err: ExecError = new Error("stdout maxBuffer length exceeded");
        err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER" as unknown as number;
        err.killed = true;
        err.cmd = command;
        err.stdout = stdout;
        err.stderr = stderr;
        callback(err, stdout, stderr);
      } else if (code !== 0) {
        const err: ExecError = new Error("Command failed: " + command);
        err.code = code;
        err.killed = false;
        err.signal = null;
        err.cmd = command;
        err.stdout = stdout;
        err.stderr = stderr;
        callback(err, stdout, stderr);
      } else {
        callback(null, stdout, stderr);
      }
    }
  });

  child.on("error", (err: unknown) => {
    if (callback) {
      const error: ExecError = err instanceof Error ? err : new Error(String(err));
      error.code = 1;
      error.stdout = stdout;
      error.stderr = stderr;
      callback(error, stdout, stderr);
    }
  });

  return child;
}

// execSync - synchronous shell execution
// Uses spawnSync("bash", ["-c", command]) internally
function execSync(
  command: string,
  options?: nodeChildProcess.ExecSyncOptions
): string | Buffer {
  const opts = options || {};

  if (typeof _childProcessSpawnSync === "undefined") {
    throw new Error("child_process.execSync requires CommandExecutor to be configured");
  }

  // Default maxBuffer 1MB (Node.js convention)
  const maxBuffer = opts.maxBuffer ?? 1024 * 1024;

  // Use synchronous bridge call - result is JSON string
  const jsonResult = _childProcessSpawnSync.applySyncPromise(undefined, [
    "bash",
    JSON.stringify(["-c", command]),
    JSON.stringify({ cwd: opts.cwd, env: opts.env as Record<string, string>, maxBuffer }),
  ]);
  const result = JSON.parse(jsonResult) as { stdout: string; stderr: string; code: number; maxBufferExceeded?: boolean };

  if (result.maxBufferExceeded) {
    const err: ExecError = new Error("stdout maxBuffer length exceeded");
    err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER" as unknown as number;
    err.stdout = result.stdout;
    err.stderr = result.stderr;
    throw err;
  }

  if (result.code !== 0) {
    const err: ExecError = new Error("Command failed: " + command);
    err.status = result.code;
    err.stdout = result.stdout;
    err.stderr = result.stderr;
    err.output = [null, result.stdout, result.stderr];
    throw err;
  }

  if (opts.encoding === "buffer" || !opts.encoding) {
    return typeof Buffer !== "undefined" ? Buffer.from(result.stdout) : (result.stdout as unknown as Buffer);
  }
  return result.stdout;
}

// spawn - spawn a command with streaming
function spawn(
  command: string,
  args?: readonly string[] | nodeChildProcess.SpawnOptions,
  options?: nodeChildProcess.SpawnOptions
): ChildProcess {
  let argsArray: string[] = [];
  let opts: nodeChildProcess.SpawnOptions = {};

  if (!Array.isArray(args)) {
    opts = (args as nodeChildProcess.SpawnOptions) || {};
  } else {
    argsArray = args as string[];
    opts = options || {};
  }

  const child = new ChildProcess();
  child.spawnfile = command;
  child.spawnargs = [command, ...argsArray];

  // Check if streaming mode is available
  if (typeof _childProcessSpawnStart !== "undefined") {
    // Use process.cwd() as default if no cwd specified
    // This ensures process.chdir() changes are reflected in child processes
    const effectiveCwd = opts.cwd ?? (typeof process !== "undefined" ? process.cwd() : "/");

    // Streaming mode - spawn immediately
    const sessionId = _childProcessSpawnStart.applySync(undefined, [
      command,
      JSON.stringify(argsArray),
      JSON.stringify({ cwd: effectiveCwd, env: opts.env }),
    ]);

    childProcessInstances.set(sessionId, child);

    // Register handle to keep sandbox alive until child exits
    // See: docs-internal/node/ACTIVE_HANDLES.md
    if (typeof _registerHandle === "function") {
      _registerHandle(`child:${sessionId}`, `child_process: ${command} ${argsArray.join(" ")}`);
    }

    // Override stdin methods for streaming
    child.stdin.write = (data: unknown): boolean => {
      if (typeof _childProcessStdinWrite === "undefined") return false;
      const bytes =
        typeof data === "string" ? new TextEncoder().encode(data) : (data as Uint8Array);
      _childProcessStdinWrite.applySync(undefined, [sessionId, bytes]);
      return true;
    };

    child.stdin.end = (): void => {
      if (typeof _childProcessStdinClose !== "undefined") {
        _childProcessStdinClose.applySync(undefined, [sessionId]);
      }
      child.stdin.writable = false;
    };

    // Override kill method
    child.kill = (signal?: NodeJS.Signals | number): boolean => {
      if (typeof _childProcessKill === "undefined") return false;
      const sig =
        signal === "SIGKILL" || signal === 9
          ? 9
          : signal === "SIGINT" || signal === 2
            ? 2
            : 15;
      _childProcessKill.applySync(undefined, [sessionId, sig]);
      child.killed = true;
      child.signalCode = (
        typeof signal === "string" ? signal : "SIGTERM"
      ) as NodeJS.Signals;
      return true;
    };

    // Emit "spawn" event asynchronously to match Node.js behavior.
    // In real Node.js, "spawn" fires on next tick after successful fork.
    child.pid = Number(sessionId) || -1;
    setTimeout(() => child.emit("spawn"), 0);

    return child;
  }

  // Fallback: no CommandExecutor available
  const err = new Error(
    "child_process.spawn requires CommandExecutor to be configured"
  );
  // Emit error asynchronously to match Node.js behavior
  setTimeout(() => {
    child.emit("error", err);
    child._complete("", err.message, 1);
  }, 0);

  return child;
}

// SpawnSyncResult type
interface SpawnSyncResult {
  pid: number;
  output: [null, string | Buffer, string | Buffer];
  stdout: string | Buffer;
  stderr: string | Buffer;
  status: number | null;
  signal: NodeJS.Signals | null;
  error?: Error;
}

// spawnSync - synchronous spawn
function spawnSync(
  command: string,
  args?: readonly string[] | nodeChildProcess.SpawnSyncOptions,
  options?: nodeChildProcess.SpawnSyncOptions
): SpawnSyncResult {
  let argsArray: string[] = [];
  let opts: nodeChildProcess.SpawnSyncOptions = {};

  if (!Array.isArray(args)) {
    opts = (args as nodeChildProcess.SpawnSyncOptions) || {};
  } else {
    argsArray = args as string[];
    opts = options || {};
  }

  if (typeof _childProcessSpawnSync === "undefined") {
    return {
      pid: _nextChildPid++,
      output: [null, "", "child_process.spawnSync requires CommandExecutor to be configured"],
      stdout: "",
      stderr: "child_process.spawnSync requires CommandExecutor to be configured",
      status: 1,
      signal: null,
      error: new Error("child_process.spawnSync requires CommandExecutor to be configured"),
    };
  }

  try {
    // Use process.cwd() as default if no cwd specified
    // This ensures process.chdir() changes are reflected in child processes
    const effectiveCwd = opts.cwd ?? (typeof process !== "undefined" ? process.cwd() : "/");

    // Pass maxBuffer through to host for enforcement
    const maxBuffer = opts.maxBuffer as number | undefined;

    // Args passed as JSON string for transferability
    const jsonResult = _childProcessSpawnSync.applySyncPromise(undefined, [
      command,
      JSON.stringify(argsArray),
      JSON.stringify({ cwd: effectiveCwd, env: opts.env as Record<string, string>, maxBuffer }),
    ]);
    const result = JSON.parse(jsonResult) as { stdout: string; stderr: string; code: number; maxBufferExceeded?: boolean };

    const stdoutBuf = typeof Buffer !== "undefined" ? Buffer.from(result.stdout) : result.stdout;
    const stderrBuf = typeof Buffer !== "undefined" ? Buffer.from(result.stderr) : result.stderr;

    if (result.maxBufferExceeded) {
      const err: ExecError = new Error("stdout maxBuffer length exceeded");
      err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER" as unknown as number;
      return {
        pid: _nextChildPid++,
        output: [null, stdoutBuf as string | Buffer, stderrBuf as string | Buffer],
        stdout: stdoutBuf as string | Buffer,
        stderr: stderrBuf as string | Buffer,
        status: result.code,
        signal: null,
        error: err,
      };
    }

    return {
      pid: _nextChildPid++,
      output: [null, stdoutBuf as string | Buffer, stderrBuf as string | Buffer],
      stdout: stdoutBuf as string | Buffer,
      stderr: stderrBuf as string | Buffer,
      status: result.code,
      signal: null,
      error: undefined,
    };
  } catch (err) {
    const errMsg = err instanceof Error ? err.message : String(err);
    const stderrBuf = typeof Buffer !== "undefined" ? Buffer.from(errMsg) : errMsg;

    return {
      pid: _nextChildPid++,
      output: [null, "", stderrBuf as string | Buffer],
      stdout: typeof Buffer !== "undefined" ? Buffer.from("") : "",
      stderr: stderrBuf as string | Buffer,
      status: 1,
      signal: null,
      error: err instanceof Error ? err : new Error(String(err)),
    };
  }
}

// execFile - execute a file directly
function execFile(
  file: string,
  args?: readonly string[] | nodeChildProcess.ExecFileOptions | ((error: ExecError | null, stdout: string, stderr: string) => void),
  options?: nodeChildProcess.ExecFileOptions | ((error: ExecError | null, stdout: string, stderr: string) => void),
  callback?: (error: ExecError | null, stdout: string, stderr: string) => void
): ChildProcess {
  let argsArray: string[] = [];
  let opts: nodeChildProcess.ExecFileOptions = {};
  let cb: ((error: ExecError | null, stdout: string, stderr: string) => void) | undefined;

  if (typeof args === "function") {
    cb = args;
  } else if (typeof options === "function") {
    argsArray = (args as readonly string[]).slice();
    cb = options;
  } else {
    argsArray = Array.isArray(args) ? (args as string[]) : [];
    opts = (options as nodeChildProcess.ExecFileOptions) || {};
    cb = callback;
  }

  // execFile is like spawn but with callback, with maxBuffer enforcement
  const maxBuffer = opts.maxBuffer ?? 1024 * 1024;
  const child = spawn(file, argsArray, opts as nodeChildProcess.SpawnOptions);

  let stdout = "";
  let stderr = "";
  let stdoutBytes = 0;
  let stderrBytes = 0;
  let maxBufferExceeded = false;

  child.stdout.on("data", (data: unknown) => {
    const chunk = String(data);
    stdout += chunk;
    stdoutBytes += chunk.length;
    if (stdoutBytes > maxBuffer && !maxBufferExceeded) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });
  child.stderr.on("data", (data: unknown) => {
    const chunk = String(data);
    stderr += chunk;
    stderrBytes += chunk.length;
    if (stderrBytes > maxBuffer && !maxBufferExceeded) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });

  child.on("close", (...args: unknown[]) => {
    const code = args[0] as number;
    if (cb) {
      if (maxBufferExceeded) {
        const err: ExecError = new Error("stdout maxBuffer length exceeded");
        err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER" as unknown as number;
        err.killed = true;
        err.stdout = stdout;
        err.stderr = stderr;
        cb(err, stdout, stderr);
      } else if (code !== 0) {
        const err: ExecError = new Error("Command failed: " + file);
        err.code = code;
        err.stdout = stdout;
        err.stderr = stderr;
        cb(err, stdout, stderr);
      } else {
        cb(null, stdout, stderr);
      }
    }
  });

  child.on("error", (err: unknown) => {
    if (cb) {
      cb(err as ExecError, stdout, stderr);
    }
  });

  return child;
}

// execFileSync
function execFileSync(
  file: string,
  args?: readonly string[] | nodeChildProcess.ExecFileSyncOptions,
  options?: nodeChildProcess.ExecFileSyncOptions
): string | Buffer {
  let argsArray: string[] = [];
  let opts: nodeChildProcess.ExecFileSyncOptions = {};

  if (!Array.isArray(args)) {
    opts = (args as nodeChildProcess.ExecFileSyncOptions) || {};
  } else {
    argsArray = args as string[];
    opts = options || {};
  }

  // Default maxBuffer 1MB for execFileSync (Node.js convention)
  const maxBuffer = opts.maxBuffer ?? 1024 * 1024;
  const result = spawnSync(file, argsArray, { ...opts, maxBuffer } as nodeChildProcess.SpawnSyncOptions);

  if (result.error && String((result.error as ExecError).code) === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER") {
    throw result.error;
  }

  if (result.status !== 0) {
    const err: ExecError = new Error("Command failed: " + file);
    err.status = result.status ?? undefined;
    err.stdout = String(result.stdout);
    err.stderr = String(result.stderr);
    throw err;
  }

  if (opts.encoding === "buffer" || !opts.encoding) {
    return result.stdout;
  }
  return typeof result.stdout === "string" ? result.stdout : result.stdout.toString(opts.encoding as BufferEncoding);
}

// fork - intentionally not implemented (IPC between processes not supported in sandbox)
function fork(
  _modulePath: string,
  _args?: readonly string[] | nodeChildProcess.ForkOptions,
  _options?: nodeChildProcess.ForkOptions
): never {
  throw new Error("child_process.fork is not supported in sandbox");
}

// Create the child_process module
const childProcess = {
  ChildProcess,
  exec,
  execSync,
  spawn,
  spawnSync,
  execFile,
  execFileSync,
  fork,
};

// Expose to global for require() to use
exposeCustomGlobal("_childProcessModule", childProcess);

export { ChildProcess, exec, execSync, spawn, spawnSync, execFile, execFileSync, fork };
export default childProcess;
