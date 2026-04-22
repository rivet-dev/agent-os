/**
 * Python runtime driver for kernel integration.
 *
 * Wraps Pyodide behind the kernel RuntimeDriver interface. Each spawn()
 * reuses a single shared Worker thread (Pyodide is expensive to load).
 * Python's os.system() and subprocess are monkey-patched to route through
 * KernelInterface.spawn() via a kernelSpawn RPC method.
 */

import { createRequire } from 'node:module';
import { dirname } from 'node:path';
import { Worker } from 'node:worker_threads';
import type {
  KernelRuntimeDriver as RuntimeDriver,
  KernelInterface,
  ProcessContext,
  DriverProcess,
} from '@secure-exec/core';
import {
  WORKER_WHEEL_PRELOAD_JS,
  normalizeWheelPreload,
  type WheelPreloadOptions,
  type WheelPreloadPayload,
} from './wheel-preload.js';

export interface PythonRuntimeOptions {
  /** CPU time limit in ms for each Python execution (no limit by default). */
  cpuTimeLimitMs?: number;
  /**
   * Pre-mount a host wheel directory into Pyodide's VFS via NODEFS and
   * micropip-install the wheels at boot. Used by the agent-os dbt-on-Pyodide
   * stack to vendor `dbt-core` + `dbt-duckdb` + `duckdb` offline.
   */
  wheelPreload?: WheelPreloadOptions;
}

/**
 * Create a Python RuntimeDriver that can be mounted into the kernel.
 */
export function createPythonRuntime(options?: PythonRuntimeOptions): RuntimeDriver {
  return new PythonRuntimeDriver(options);
}

// ---------------------------------------------------------------------------
// Pyodide index path resolution
// ---------------------------------------------------------------------------

let _indexPathCache: string | null = null;
let _moduleUrlCache: string | null = null;

function getPyodideIndexPath(): string {
  if (_indexPathCache) return _indexPathCache;
  const requireFromRuntime = createRequire(import.meta.url);
  const pyodideModulePath = requireFromRuntime.resolve('pyodide/pyodide.mjs');
  _indexPathCache = `${dirname(pyodideModulePath)}/`;
  return _indexPathCache;
}

/**
 * Resolve the absolute file:// URL for `pyodide.mjs`. Used by the worker
 * which (running with eval:true) cannot resolve bare specifiers like
 * "pyodide" from its own context. The host resolves the path here and
 * forwards the URL through the init payload.
 */
function getPyodideModuleUrl(): string {
  if (_moduleUrlCache) return _moduleUrlCache;
  const requireFromRuntime = createRequire(import.meta.url);
  const pyodideModulePath = requireFromRuntime.resolve('pyodide/pyodide.mjs');
  _moduleUrlCache = `file://${pyodideModulePath}`;
  return _moduleUrlCache;
}

// ---------------------------------------------------------------------------
// Worker RPC message types
// ---------------------------------------------------------------------------

type WorkerRequestMessage = {
  id: number;
  type: 'init' | 'spawn';
  payload?: unknown;
};

type WorkerResponseMessage = {
  type: 'response';
  id: number;
  ok: boolean;
  result?: unknown;
  error?: { message: string; stack?: string };
};

type WorkerStdioMessage = {
  type: 'stdio';
  requestId: number;
  channel: 'stdout' | 'stderr';
  message: string;
};

type WorkerRpcMessage = {
  type: 'rpc';
  id: number;
  method: string;
  params: Record<string, unknown>;
};

type WorkerOutboundMessage =
  | WorkerResponseMessage
  | WorkerStdioMessage
  | WorkerRpcMessage;

type WorkerRpcResultMessage = {
  type: 'rpcResult';
  id: number;
  ok: boolean;
  result?: unknown;
  error?: { message: string };
};

type PendingRequest = {
  resolve(value: unknown): void;
  reject(reason: unknown): void;
  /** Callbacks for stdio routing */
  onStdout?: (data: Uint8Array) => void;
  onStderr?: (data: Uint8Array) => void;
};

// ---------------------------------------------------------------------------
// Inline worker source — loaded via eval
// ---------------------------------------------------------------------------

const WORKER_SOURCE = String.raw`
const { parentPort } = require("node:worker_threads");

let pyodide = null;
let currentRequestId = null;
let nextRpcId = 1;
const pendingRpc = new Map();
${WORKER_WHEEL_PRELOAD_JS}

function serializeError(error) {
  if (error instanceof Error) {
    return { message: error.message, stack: error.stack };
  }
  return { message: String(error) };
}

function postStdio(channel, message) {
  if (currentRequestId === null) return;
  parentPort.postMessage({
    type: "stdio",
    requestId: currentRequestId,
    channel,
    message: String(message),
  });
}

function callHost(method, params) {
  return new Promise((resolve, reject) => {
    const id = nextRpcId++;
    pendingRpc.set(id, { resolve, reject });
    parentPort.postMessage({ type: "rpc", id, method, params });
  });
}

async function ensurePyodide(payload) {
  if (pyodide) return pyodide;
  // Pyodide must be loaded by absolute file: URL, not by bare specifier.
  // The Worker is created with eval:true and has no module-resolution cwd,
  // so bare specifiers fail when this driver is mounted from a different
  // package (e.g. agent-os-core consuming agent-os-python). The host
  // resolves pyodide.mjs's location via createRequire and passes it here.
  const pyodideUrl = payload?.pyodideModuleUrl
    ? payload.pyodideModuleUrl
    : "pyodide";
  const { loadPyodide } = await import(pyodideUrl);
  pyodide = await loadPyodide({
    indexURL: payload?.indexPath,
    env: payload?.env || {},
    stdout: (message) => postStdio("stdout", message),
    stderr: (message) => postStdio("stderr", message),
  });

  // Register host RPC bridge
  pyodide.registerJsModule("secure_exec", {
    read_text_file: async (path) => callHost("fsReadTextFile", { path }),
    fetch: async (url, options) =>
      callHost("networkFetch", { url, options: options || {} }),
    kernel_spawn: async (command, argsJson, envJson, cwd) =>
      callHost("kernelSpawn", { command, args: JSON.parse(argsJson), env: JSON.parse(envJson), cwd }),
  });

  // Preload vendored wheels (mount + micropip + bootstrap) BEFORE installing
  // the import sandbox. micropip itself imports pyodide_js, which the sandbox
  // would otherwise reject. The trusted preload code runs first, then we
  // engage the import block for subsequent agent code.
  await applyWheelPreload(pyodide, payload?.wheelPreload);

  // Block import js / pyodide_js — prevents sandbox escape via host JS runtime
  await pyodide.runPythonAsync([
    "import sys",
    "import importlib.abc",
    "import importlib.machinery",
    "class _BlockHostJsImporter(importlib.abc.MetaPathFinder):",
    "    _BLOCKED = frozenset(('js', 'pyodide_js'))",
    "    def find_spec(self, fullname, path, target=None):",
    "        if fullname in self._BLOCKED or fullname.startswith('js.') or fullname.startswith('pyodide_js.'):",
    "            raise ImportError('module ' + repr(fullname) + ' is blocked in sandbox')",
    "        return None",
    "    def find_module(self, fullname, path=None):",
    "        if fullname in self._BLOCKED or fullname.startswith('js.') or fullname.startswith('pyodide_js.'):",
    "            raise ImportError('module ' + repr(fullname) + ' is blocked in sandbox')",
    "        return None",
    "sys.meta_path.insert(0, _BlockHostJsImporter())",
    "for _m in list(sys.modules):",
    "    if _m == 'js' or _m == 'pyodide_js' or _m.startswith('js.') or _m.startswith('pyodide_js.'):",
    "        del sys.modules[_m]",
    "del _m, _BlockHostJsImporter",
  ].join("\n"));

  return pyodide;
}

// Monkey-patch os.system and subprocess to route through kernel
const KERNEL_SPAWN_PATCH = String.raw` + "`" + String.raw`
import secure_exec as _se
import os as _os
import json as _json

def _kernel_system(cmd):
    """Route os.system() through kernel via RPC."""
    try:
        result = _se.kernel_spawn('sh', _json.dumps(['-c', cmd]), _json.dumps(dict(_os.environ)), _os.getcwd())
        # kernel_spawn returns exit code
        return int(result) if result is not None else 0
    except Exception:
        return 1

_os.system = _kernel_system

# Monkey-patch subprocess module
import subprocess as _subprocess
import sys as _sys

class _KernelPopen:
    """Minimal Popen replacement that routes through kernel."""
    def __init__(self, args, stdin=None, stdout=None, stderr=None, shell=False, env=None, cwd=None, **kwargs):
        if shell and isinstance(args, str):
            self._command = 'sh'
            self._args = ['-c', args]
        elif isinstance(args, str):
            self._command = args
            self._args = []
        else:
            args = list(args)
            self._command = args[0] if args else ''
            self._args = args[1:] if len(args) > 1 else []

        self._env = dict(env) if env else dict(_os.environ)
        self._cwd = cwd or _os.getcwd()
        self._stdin_data = None
        self._capture_stdout = stdout == _subprocess.PIPE
        self._capture_stderr = stderr == _subprocess.PIPE
        self.returncode = None
        self.stdout = None
        self.stderr = None

        if stdin == _subprocess.PIPE:
            self._stdin_data = b''

    def communicate(self, input=None, timeout=None):
        try:
            result = _se.kernel_spawn(
                self._command,
                _json.dumps(self._args),
                _json.dumps(self._env),
                self._cwd,
            )
            self.returncode = int(result) if result is not None else 0
        except Exception:
            self.returncode = 1

        stdout = b'' if self._capture_stdout else None
        stderr = b'' if self._capture_stderr else None
        return (stdout, stderr)

    def wait(self, timeout=None):
        if self.returncode is None:
            self.communicate()
        return self.returncode

    def poll(self):
        return self.returncode

    def kill(self):
        pass

    def terminate(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

_subprocess.Popen = _KernelPopen

_original_run = _subprocess.run

def _kernel_run(args, **kwargs):
    p = _KernelPopen(args, **kwargs)
    stdout, stderr = p.communicate(kwargs.get('input'))
    cp = _subprocess.CompletedProcess(args, p.returncode, stdout, stderr)
    if kwargs.get('check') and p.returncode != 0:
        raise _subprocess.CalledProcessError(p.returncode, args, stdout, stderr)
    return cp

_subprocess.run = _kernel_run

def _kernel_call(args, **kwargs):
    p = _KernelPopen(args, **kwargs)
    p.communicate()
    return p.returncode

_subprocess.call = _kernel_call

def _kernel_check_call(args, **kwargs):
    rc = _kernel_call(args, **kwargs)
    if rc != 0:
        raise _subprocess.CalledProcessError(rc, args)
    return 0

_subprocess.check_call = _kernel_check_call

def _kernel_check_output(args, **kwargs):
    kwargs['stdout'] = _subprocess.PIPE
    cp = _kernel_run(args, **kwargs)
    return cp.stdout

_subprocess.check_output = _kernel_check_output
` + "`" + String.raw`;

async function applyExecOverrides(py, options) {
  if (!options) {
    py.setStdin();
    // Reset argv to a conservative default so a prior invocation's argv
    // never bleeds into the next one.
    await py.runPythonAsync("import sys\nsys.argv = ['']");
    return;
  }
  if (typeof options.stdin === "string") {
    const lines = options.stdin.split(/\r?\n/);
    let cursor = 0;
    py.setStdin({
      stdin: () => {
        if (cursor >= lines.length) return undefined;
        return lines[cursor++];
      },
      autoEOF: true,
    });
  } else {
    py.setStdin();
  }

  if (options.env && typeof options.env === "object" && Object.keys(options.env).length > 0) {
    py.globals.set("__secure_exec_env_json__", JSON.stringify(options.env));
    try {
      await py.runPythonAsync(
        "import json\nimport os\nfor _k, _v in json.loads(__secure_exec_env_json__).items():\n    os.environ[str(_k)] = str(_v)"
      );
    } finally {
      try { py.globals.delete("__secure_exec_env_json__"); } catch {}
    }
  }

  if (typeof options.cwd === "string") {
    py.globals.set("__secure_exec_cwd__", options.cwd);
    try {
      await py.runPythonAsync("import os\ntry:\n    os.chdir(str(__secure_exec_cwd__))\nexcept OSError:\n    pass");
    } finally {
      try { py.globals.delete("__secure_exec_cwd__"); } catch {}
    }
  }

  // Set sys.argv from the JSON payload so Python scripts see the caller's
  // args. Matches CPython contract:
  //   - 'python script.py a b'  -> argv = ['script.py', 'a', 'b']
  //   - 'python -c "..." a b'   -> argv = ['-c', 'a', 'b']
  //   - 'python -m mod a b'     -> argv = ['mod', 'a', 'b']
  // Always set this (including to [""]) so leftover state from a previous
  // invocation can't leak into the next one.
  const argv = Array.isArray(options.argv) ? options.argv : [""];
  py.globals.set("__secure_exec_argv_json__", JSON.stringify(argv));
  try {
    await py.runPythonAsync(
      "import json, sys\nsys.argv = [str(_a) for _a in json.loads(__secure_exec_argv_json__)]"
    );
  } finally {
    try { py.globals.delete("__secure_exec_argv_json__"); } catch {}
  }
}

parentPort.on("message", async (message) => {
  if (!message || typeof message !== "object") return;

  // Handle RPC result from host
  if (message.type === "rpcResult") {
    const pending = pendingRpc.get(message.id);
    if (!pending) return;
    pendingRpc.delete(message.id);
    if (message.ok) {
      pending.resolve(message.result);
    } else {
      pending.reject(new Error(message.error?.message || "Host RPC failed"));
    }
    return;
  }

  if (message.type !== "init" && message.type !== "spawn") return;

  currentRequestId = message.id;
  try {
    const py = await ensurePyodide(
      message.type === "init" ? message.payload : undefined
    );

    if (message.type === "init") {
      // Apply kernel spawn monkey-patches
      await py.runPythonAsync(KERNEL_SPAWN_PATCH);
      parentPort.postMessage({ type: "response", id: message.id, ok: true, result: {} });
      return;
    }

    // spawn: run Python code
    const payload = message.payload || {};
    await applyExecOverrides(py, payload.options);

    try {
      await py.runPythonAsync(payload.code, {
        filename: payload.filePath || "<exec>",
      });
      parentPort.postMessage({
        type: "response",
        id: message.id,
        ok: true,
        result: { exitCode: 0 },
      });
    } catch (error) {
      // Check for SystemExit
      const msg = error?.message || String(error);
      const exitMatch = msg.match(/SystemExit:\s*(\d+)/);
      if (exitMatch) {
        parentPort.postMessage({
          type: "response",
          id: message.id,
          ok: true,
          result: { exitCode: parseInt(exitMatch[1], 10) },
        });
      } else {
        parentPort.postMessage({
          type: "response",
          id: message.id,
          ok: true,
          result: { exitCode: 1, error: msg },
        });
        // Also emit the error to stderr
        postStdio("stderr", msg);
      }
    }
  } catch (error) {
    parentPort.postMessage({
      type: "response",
      id: message.id,
      ok: false,
      error: serializeError(error),
    });
  } finally {
    currentRequestId = null;
  }
});
`;

// ---------------------------------------------------------------------------
// PythonRuntimeDriver
// ---------------------------------------------------------------------------

class PythonRuntimeDriver implements RuntimeDriver {
  readonly name = 'python';
  readonly commands: string[] = ['python', 'python3', 'pip'];

  private _kernel: KernelInterface | null = null;
  private _worker: Worker | null = null;
  private _readyPromise: Promise<void> | null = null;
  private _disposed = false;
  private _nextRequestId = 1;
  private _pending = new Map<number, PendingRequest>();
  private _cpuTimeLimitMs?: number;
  private _wheelPreload?: WheelPreloadPayload;

  constructor(options?: PythonRuntimeOptions) {
    this._cpuTimeLimitMs = options?.cpuTimeLimitMs;
    this._wheelPreload = normalizeWheelPreload(options?.wheelPreload);
  }

  async init(kernel: KernelInterface): Promise<void> {
    this._kernel = kernel;
  }

  spawn(command: string, args: string[], ctx: ProcessContext): DriverProcess {
    const kernel = this._kernel;
    if (!kernel) throw new Error('Python driver not initialized');

    // Exit plumbing
    let resolveExit!: (code: number) => void;
    let exitResolved = false;
    const exitPromise = new Promise<number>((resolve) => {
      resolveExit = (code: number) => {
        if (exitResolved) return;
        exitResolved = true;
        resolve(code);
      };
    });

    const proc: DriverProcess = {
      onStdout: null,
      onStderr: null,
      onExit: null,
      writeStdin: (_data: Uint8Array) => {
        // Pyodide stdin is set per-execution, not streamed
      },
      closeStdin: () => {},
      kill: (_signal: number) => {
        // Terminate the worker to kill the process
        if (this._worker) {
          this._worker.removeAllListeners();
          void this._worker.terminate();
          this._worker = null;
          this._readyPromise = null;
          this._rejectAllPending(new Error('Process killed'));
        }
      },
      wait: () => exitPromise,
    };

    // Launch async — spawn() returns synchronously per RuntimeDriver contract
    this._executeAsync(command, args, ctx, proc, resolveExit);

    return proc;
  }

  async dispose(): Promise<void> {
    if (this._disposed) return;
    this._disposed = true;
    const worker = this._worker;
    this._worker = null;
    this._readyPromise = null;
    this._kernel = null;
    if (worker) {
      worker.removeAllListeners();
      await worker.terminate();
    }
    this._rejectAllPending(new Error('Python driver disposed'));
  }

  // -------------------------------------------------------------------------
  // Worker lifecycle
  // -------------------------------------------------------------------------

  private async _ensureWorkerReady(): Promise<void> {
    if (this._disposed) throw new Error('Python driver disposed');
    if (this._readyPromise) {
      await this._readyPromise;
      return;
    }

    this._worker = this._createWorker();
    const indexPath = getPyodideIndexPath();
    const pyodideModuleUrl = getPyodideModuleUrl();
    this._readyPromise = this._callWorker<void>('init', {
      indexPath,
      pyodideModuleUrl,
      env: {},
      wheelPreload: this._wheelPreload,
    }).then(() => undefined);
    await this._readyPromise;
  }

  private _createWorker(): Worker {
    const worker = new Worker(WORKER_SOURCE, { eval: true });
    worker.on('message', this._handleWorkerMessage);
    worker.on('error', this._handleWorkerError);
    worker.on('exit', this._handleWorkerExit);
    return worker;
  }

  // -------------------------------------------------------------------------
  // Worker message handling
  // -------------------------------------------------------------------------

  private _handleWorkerMessage = (message: WorkerOutboundMessage): void => {
    if (message.type === 'stdio') {
      const pending = this._pending.get(message.requestId);
      if (!pending) return;
      const data = new TextEncoder().encode(message.message + '\n');
      if (message.channel === 'stdout') {
        pending.onStdout?.(data);
      } else {
        pending.onStderr?.(data);
      }
      return;
    }

    if (message.type === 'rpc') {
      void this._handleWorkerRpc(message);
      return;
    }

    // Response message
    const pending = this._pending.get(message.id);
    if (!pending) return;
    this._pending.delete(message.id);
    if (message.ok) {
      pending.resolve(message.result);
    } else {
      const error = new Error(message.error?.message ?? 'Pyodide worker request failed');
      if (message.error?.stack) error.stack = message.error.stack;
      pending.reject(error);
    }
  };

  private _handleWorkerError = (error: Error): void => {
    this._rejectAllPending(error);
  };

  private _handleWorkerExit = (): void => {
    if (!this._disposed) {
      this._rejectAllPending(new Error('Pyodide worker exited unexpectedly'));
    }
    this._worker = null;
    this._readyPromise = null;
  };

  private async _handleWorkerRpc(message: WorkerRpcMessage): Promise<void> {
    const kernel = this._kernel;
    if (!kernel || !this._worker) return;

    let result: unknown;
    let error: Error | null = null;

    try {
      switch (message.method) {
        case 'fsReadTextFile': {
          const path = String(message.params.path ?? '');
          result = await kernel.vfs.readTextFile(path);
          break;
        }
        case 'kernelSpawn': {
          const command = String(message.params.command ?? '');
          const spawnArgs = (message.params.args as string[]) ?? [];
          const env = (message.params.env as Record<string, string>) ?? {};
          const cwd = String(message.params.cwd ?? '/');

          // Route through kernel — dispatches to WasmVM/Node/other drivers
          const managed = kernel.spawn(command, spawnArgs, {
            env,
            cwd,
            onStdout: (data) => {
              // Forward child stdout to this process's stdout
              const pending = this._findPendingForRpc(message);
              pending?.onStdout?.(data);
            },
            onStderr: (data) => {
              const pending = this._findPendingForRpc(message);
              pending?.onStderr?.(data);
            },
          });

          const exitCode = await managed.wait();
          result = exitCode;
          break;
        }
        default:
          throw new Error(`Unsupported worker RPC method: ${message.method}`);
      }
    } catch (rpcError) {
      error = rpcError instanceof Error ? rpcError : new Error(String(rpcError));
    }

    if (!this._worker) return;

    const response: WorkerRpcResultMessage = error
      ? { type: 'rpcResult', id: message.id, ok: false, error: { message: error.message } }
      : { type: 'rpcResult', id: message.id, ok: true, result };
    this._worker.postMessage(response);
  }

  /**
   * Find the pending request that corresponds to the current spawn execution.
   * RPC calls happen during a spawn, so find the spawn request.
   */
  private _findPendingForRpc(_rpcMessage: WorkerRpcMessage): PendingRequest | undefined {
    // The most recent spawn request is the active one
    for (const pending of this._pending.values()) {
      if (pending.onStdout || pending.onStderr) return pending;
    }
    return undefined;
  }

  // -------------------------------------------------------------------------
  // Worker call helper
  // -------------------------------------------------------------------------

  private _callWorker<T>(
    type: 'init' | 'spawn',
    payload?: unknown,
    onStdout?: (data: Uint8Array) => void,
    onStderr?: (data: Uint8Array) => void,
  ): Promise<T> {
    if (this._disposed) return Promise.reject(new Error('Python driver disposed'));
    if (!this._worker) return Promise.reject(new Error('Pyodide worker is not initialized'));

    const id = this._nextRequestId++;
    const message: WorkerRequestMessage =
      payload === undefined ? { id, type } : { id, type, payload };

    return new Promise<T>((resolve, reject) => {
      this._pending.set(id, { resolve, reject, onStdout, onStderr });
      this._worker!.postMessage(message);
    });
  }

  private _rejectAllPending(error: Error): void {
    const pendingRequests = Array.from(this._pending.values());
    this._pending.clear();
    for (const pending of pendingRequests) {
      pending.reject(error);
    }
  }

  // -------------------------------------------------------------------------
  // Async execution
  // -------------------------------------------------------------------------

  private async _executeAsync(
    command: string,
    args: string[],
    ctx: ProcessContext,
    proc: DriverProcess,
    resolveExit: (code: number) => void,
  ): Promise<void> {
    const kernel = this._kernel!;

    try {
      // Ensure Pyodide worker is loaded
      await this._ensureWorkerReady();

      // Resolve the Python code to execute
      const { code, filePath, argv } = await this._resolveEntry(command, args, kernel);

      // Build stdout/stderr forwarders
      const onStdout = (data: Uint8Array) => {
        ctx.onStdout?.(data);
        proc.onStdout?.(data);
      };
      const onStderr = (data: Uint8Array) => {
        ctx.onStderr?.(data);
        proc.onStderr?.(data);
      };

      // Execute via worker
      const result = await this._callWorker<{ exitCode: number; error?: string }>(
        'spawn',
        {
          code,
          filePath,
          options: {
            env: ctx.env,
            cwd: ctx.cwd,
            argv,
          },
        },
        onStdout,
        onStderr,
      );

      const exitCode = result?.exitCode ?? 0;
      resolveExit(exitCode);
      proc.onExit?.(exitCode);
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : String(err);
      const errBytes = new TextEncoder().encode(`python: ${errMsg}\n`);
      ctx.onStderr?.(errBytes);
      proc.onStderr?.(errBytes);

      resolveExit(1);
      proc.onExit?.(1);
    }
  }

  // -------------------------------------------------------------------------
  // Entry point resolution
  // -------------------------------------------------------------------------

  /**
   * Resolve the Python code to execute from command + args.
   * - 'python script.py' -> read script from VFS
   * - 'python -c "code"' -> inline code
   * - 'python3' -> alias for 'python'
   * - 'pip install ...' -> error (not supported)
   */
  private async _resolveEntry(
    command: string,
    args: string[],
    kernel: KernelInterface,
  ): Promise<{ code: string; filePath?: string; argv: string[] }> {
    // pip command
    if (command === 'pip') {
      throw new Error('Python package installation is not supported in this runtime');
    }

    // python / python3 — parse args
    return this._resolvePythonArgs(args, kernel);
  }

  private async _resolvePythonArgs(
    args: string[],
    kernel: KernelInterface,
  ): Promise<{ code: string; filePath?: string; argv: string[] }> {
    for (let i = 0; i < args.length; i++) {
      const arg = args[i];

      // -c: next arg is code. CPython contract: argv = ['-c', ...rest]
      if (arg === '-c' && i + 1 < args.length) {
        const scriptArgs = args.slice(i + 2);
        return { code: args[i + 1], argv: ['-c', ...scriptArgs] };
      }

      // -m: module execution. CPython contract: argv = ['<module>', ...rest]
      if (arg === '-m' && i + 1 < args.length) {
        const moduleName = args[i + 1];
        const scriptArgs = args.slice(i + 2);
        return {
          code: `import runpy; runpy.run_module('${moduleName}', run_name='__main__')`,
          argv: [moduleName, ...scriptArgs],
        };
      }

      // Skip flags
      if (arg.startsWith('-')) continue;

      // First non-flag arg is the script path. CPython contract:
      // argv = [<script_path>, ...args_after_script]
      const scriptPath = arg;
      const scriptArgs = args.slice(i + 1);
      try {
        const content = await kernel.vfs.readTextFile(scriptPath);
        return {
          code: content,
          filePath: scriptPath,
          argv: [scriptPath, ...scriptArgs],
        };
      } catch {
        throw new Error(`python: can't open file '${scriptPath}': [Errno 2] No such file or directory`);
      }
    }

    // No script or -c flag — interactive mode not supported
    throw new Error('python: missing script argument (interactive mode not supported)');
  }
}
