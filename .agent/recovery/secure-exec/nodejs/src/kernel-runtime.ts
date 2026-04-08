/**
 * Node.js runtime driver for kernel integration.
 *
 * Wraps the existing NodeExecutionDriver behind the kernel RuntimeDriver
 * interface. Each spawn() creates a fresh V8 isolate via NodeExecutionDriver
 * and executes the target script. The bridge child_process.spawn routes
 * through KernelInterface.spawn() so shell commands dispatch to WasmVM
 * or other mounted runtimes.
 */

import { existsSync, readFileSync, realpathSync } from 'node:fs';
import * as fsPromises from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import type {
  KernelRuntimeDriver as RuntimeDriver,
  KernelInterface,
  ProcessContext,
  DriverProcess,
  Permissions,
  VirtualFileSystem,
} from '@secure-exec/core';
import { NodeExecutionDriver } from './execution-driver.js';
import { createDefaultNetworkAdapter, createNodeDriver } from './driver.js';
import { transformSourceForRequireSync } from './module-source.js';
import type { BindingTree } from './bindings.js';
import {
  allowAll,
  allowAllChildProcess,
  allowAllFs,
  allowAllNetwork,
  createProcessScopedFileSystem,
} from '@secure-exec/core';
import type {
  CommandExecutor,
} from '@secure-exec/core';
import type { LiveStdinSource } from './isolate-bootstrap.js';

export interface NodeRuntimeOptions {
  /** Memory limit in MB for each V8 isolate (default: 128). */
  memoryLimit?: number;
  /**
   * Host filesystem paths that the isolate may read for module resolution
   * (e.g. npm's own install directory). By default, the driver discovers
   * the host npm location automatically.
   */
  moduleAccessPaths?: string[];
  /**
   * Bridge permissions for isolate processes. Defaults to allowAllChildProcess
   * plus read-only `/proc/self` metadata access in kernel-mounted mode
   * (other fs/network/env access deny-by-default). Use allowAll for full
   * sandbox access.
   */
  permissions?: Partial<Permissions>;
  /**
   * Host-side functions exposed to sandbox code via SecureExec.bindings.
   * Nested objects become dot-separated paths (max depth 4, max 64 leaves).
   */
  bindings?: BindingTree;
  /**
   * Loopback ports to exempt from SSRF checks. Useful for testing with
   * host-side mock servers that sandbox code needs to reach.
   */
  loopbackExemptPorts?: number[];
  /**
   * Host-side CWD for module access resolution. When set, the
   * ModuleAccessFileSystem uses this path instead of the VM process CWD
   * to locate host node_modules. Defaults to the VM process CWD.
   */
  moduleAccessCwd?: string;
  /**
   * Explicit host-to-VM path mappings from packages. These are checked
   * before the CWD-based node_modules fallback in the ModuleAccessFileSystem.
   */
  packageRoots?: Array<{ hostPath: string; vmPath: string }>;
}

const allowKernelProcSelfRead: Pick<Permissions, 'fs'> = {
  fs: (request) => {
    const rawPath = typeof request?.path === 'string' ? request.path : '';
    const normalized = rawPath.length > 1 && rawPath.endsWith('/')
      ? rawPath.slice(0, -1)
      : rawPath || '/';

    switch (request?.op) {
      case 'read':
      case 'readdir':
      case 'readlink':
      case 'stat':
      case 'exists':
        break;
      default:
        return {
          allow: false,
          reason: 'kernel procfs metadata is read-only',
        };
    }

    if (
      normalized === '/proc' ||
      normalized === '/proc/self' ||
      normalized.startsWith('/proc/self/') ||
      normalized === '/proc/sys' ||
      normalized === '/proc/sys/kernel' ||
      normalized === '/proc/sys/kernel/hostname' ||
      normalized === '/root' ||
      normalized === '/root/node_modules' ||
      normalized.startsWith('/root/node_modules/')
    ) {
      return { allow: true };
    }

    return {
      allow: false,
      reason: 'kernel-mounted Node only allows read-only /proc/self metadata by default',
    };
  },
};

/**
 * Create a Node.js RuntimeDriver that can be mounted into the kernel.
 */
export function createNodeRuntime(options?: NodeRuntimeOptions): RuntimeDriver {
  return new NodeRuntimeDriver(options);
}

// ---------------------------------------------------------------------------
// npm/npx host entry-point resolution
// ---------------------------------------------------------------------------

/** Cached result of npm entry script resolution. */
let _npmEntryCache: string | null = null;

/**
 * Resolve the npm CLI entry script on the host filesystem.
 * Walks up from `process.execPath` (the Node binary) to find the npm
 * package, then returns the path to `npm-cli.js`.
 */
function resolveNpmEntry(): string {
  if (_npmEntryCache) return _npmEntryCache;

  // Strategy 1: resolve from node's prefix (works for most installs)
  const nodeDir = dirname(process.execPath);
  const candidates = [
    // nvm / standard installs: <prefix>/lib/node_modules/npm/bin/npm-cli.js
    join(nodeDir, '..', 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    // Homebrew / some Linux layouts
    join(nodeDir, '..', 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.cjs'),
    // Windows
    join(nodeDir, 'node_modules', 'npm', 'bin', 'npm-cli.js'),
  ];

  for (const candidate of candidates) {
    const resolved = resolve(candidate);
    if (existsSync(resolved)) {
      _npmEntryCache = resolved;
      return resolved;
    }
  }

  // Strategy 2: require.resolve from the host
  try {
    const npmPkg = require.resolve('npm/package.json', { paths: [nodeDir] });
    const entry = join(dirname(npmPkg), 'bin', 'npm-cli.js');
    if (existsSync(entry)) {
      _npmEntryCache = entry;
      return entry;
    }
  } catch {
    // fall through
  }

  throw new Error(
    'Could not resolve npm CLI entry script. Searched:\n' +
    candidates.map(c => `  - ${resolve(c)}`).join('\n'),
  );
}

/** Cached result of npx entry script resolution. */
let _npxEntryCache: string | null = null;

function resolveNpxEntry(): string {
  if (_npxEntryCache) return _npxEntryCache;

  const npmEntry = resolveNpmEntry();
  const npmBinDir = dirname(npmEntry);
  const candidates = [
    join(npmBinDir, 'npx-cli.js'),
    join(npmBinDir, 'npx-cli.cjs'),
  ];

  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      _npxEntryCache = candidate;
      return candidate;
    }
  }

  throw new Error(
    'Could not resolve npx CLI entry script. Searched:\n' +
    candidates.map(c => `  - ${c}`).join('\n'),
  );
}

// ---------------------------------------------------------------------------
// KernelCommandExecutor — routes child_process.spawn through the kernel
// ---------------------------------------------------------------------------

/**
 * CommandExecutor adapter that wraps KernelInterface.spawn().
 * This is the critical integration point: when code inside the V8 isolate
 * calls child_process.spawn('sh', ['-c', 'echo hello']), the bridge
 * delegates here, which calls kernel.spawn() to route 'sh' to WasmVM.
 */
export function createKernelCommandExecutor(kernel: KernelInterface, parentPid: number): CommandExecutor {
  return {
    spawn(
      command: string,
      args: string[],
      options: {
        cwd?: string;
        env?: Record<string, string>;
        streamStdin?: boolean;
        onStdout?: (data: Uint8Array) => void;
        onStderr?: (data: Uint8Array) => void;
      },
    ) {
      // Route through kernel — this dispatches to WasmVM for shell commands,
      // other Node instances for node commands, etc.
      const managed = kernel.spawn(command, args, {
        ppid: parentPid,
        env: options.env ?? {},
        cwd: options.cwd ?? kernel.getcwd(parentPid),
        streamStdin: options.streamStdin,
        onStdout: options.onStdout,
        onStderr: options.onStderr,
      });

      return {
        writeStdin(data: Uint8Array | string): void {
          managed.writeStdin(data);
        },
        closeStdin(): void {
          managed.closeStdin();
        },
        kill(signal?: number): void {
          managed.kill(signal);
        },
        wait(): Promise<number> {
          return managed.wait();
        },
      };
    },
  };
}

// ---------------------------------------------------------------------------
// Kernel VFS adapter — adapts kernel VFS to secure-exec VirtualFileSystem
// ---------------------------------------------------------------------------

/**
 * Thin adapter from kernel VFS to secure-exec VFS interface.
 * The kernel VFS is a superset, so this just narrows the type.
 */
export function createKernelVfsAdapter(kernelVfs: KernelInterface['vfs']): VirtualFileSystem {
  return {
    readFile: (path) => kernelVfs.readFile(path),
    readTextFile: (path) => kernelVfs.readTextFile(path),
    readDir: (path) => kernelVfs.readDir(path),
    readDirWithTypes: (path) => kernelVfs.readDirWithTypes(path),
    writeFile: (path, content) => kernelVfs.writeFile(path, content),
    createDir: (path) => kernelVfs.createDir(path),
    mkdir: (path, options?) => kernelVfs.mkdir(path, options),
    exists: (path) => kernelVfs.exists(path),
    stat: (path) => kernelVfs.stat(path),
    removeFile: (path) => kernelVfs.removeFile(path),
    removeDir: (path) => kernelVfs.removeDir(path),
    rename: (oldPath, newPath) => kernelVfs.rename(oldPath, newPath),
    symlink: (target, linkPath) => kernelVfs.symlink(target, linkPath),
    readlink: (path) => kernelVfs.readlink(path),
    lstat: (path) => kernelVfs.lstat(path),
    link: (oldPath, newPath) => kernelVfs.link(oldPath, newPath),
    chmod: (path, mode) => kernelVfs.chmod(path, mode),
    chown: (path, uid, gid) => kernelVfs.chown(path, uid, gid),
    utimes: (path, atime, mtime) => kernelVfs.utimes(path, atime, mtime),
    truncate: (path, length) => kernelVfs.truncate(path, length),
    realpath: (path) => kernelVfs.realpath(path),
    pread: (path, offset, length) => kernelVfs.pread(path, offset, length),
    pwrite: (path, offset, data) => kernelVfs.pwrite(path, offset, data),
  };
}

// ---------------------------------------------------------------------------
// Host filesystem fallback — npm/npx module resolution
// ---------------------------------------------------------------------------

/**
 * Wrap a VFS with host filesystem fallback for read operations.
 *
 * When npm/npx runs inside the V8 isolate, require() must resolve npm's own
 * internal modules (e.g. '../lib/cli/entry'). These live on the host
 * filesystem, not in the kernel VFS. This wrapper tries the kernel VFS first
 * and falls back to the host filesystem for reads. Writes always go to the
 * kernel VFS.
 */
export function createHostFallbackVfs(base: VirtualFileSystem): VirtualFileSystem {
  return {
    readFile: async (path) => {
      try { return await base.readFile(path); }
      catch { return new Uint8Array(await fsPromises.readFile(path)); }
    },
    readTextFile: async (path) => {
      try { return await base.readTextFile(path); }
      catch { return await fsPromises.readFile(path, 'utf-8'); }
    },
    readDir: async (path) => {
      try { return await base.readDir(path); }
      catch { return await fsPromises.readdir(path); }
    },
    readDirWithTypes: async (path) => {
      try { return await base.readDirWithTypes(path); }
      catch {
        const entries = await fsPromises.readdir(path, { withFileTypes: true });
        return entries.map(e => ({ name: e.name, isDirectory: e.isDirectory() }));
      }
    },
    exists: async (path) => {
      if (await base.exists(path)) return true;
      try { await fsPromises.access(path); return true; } catch { return false; }
    },
    stat: async (path) => {
      try { return await base.stat(path); }
      catch {
        const s = await fsPromises.stat(path);
        return {
          mode: s.mode,
          size: s.size,
          isDirectory: s.isDirectory(),
          isSymbolicLink: false,
          atimeMs: s.atimeMs,
          mtimeMs: s.mtimeMs,
          ctimeMs: s.ctimeMs,
          birthtimeMs: s.birthtimeMs,
          ino: s.ino,
          nlink: s.nlink,
          uid: s.uid,
          gid: s.gid,
        };
      }
    },
    writeFile: (path, content) => base.writeFile(path, content),
    createDir: (path) => base.createDir(path),
    mkdir: (path, options?) => base.mkdir(path, options),
    removeFile: (path) => base.removeFile(path),
    removeDir: (path) => base.removeDir(path),
    rename: (oldPath, newPath) => base.rename(oldPath, newPath),
    symlink: (target, linkPath) => base.symlink(target, linkPath),
    readlink: (path) => base.readlink(path),
    lstat: (path) => base.lstat(path),
    link: (oldPath, newPath) => base.link(oldPath, newPath),
    chmod: (path, mode) => base.chmod(path, mode),
    chown: (path, uid, gid) => base.chown(path, uid, gid),
    utimes: (path, atime, mtime) => base.utimes(path, atime, mtime),
    truncate: (path, length) => base.truncate(path, length),
    realpath: async (path) => {
      try { return await base.realpath(path); }
      catch { return await fsPromises.realpath(path); }
    },
    pread: async (path, offset, length) => {
      try { return await base.pread(path, offset, length); }
      catch {
        const handle = await fsPromises.open(path, 'r');
        try {
          const buf = new Uint8Array(length);
          const { bytesRead } = await handle.read(buf, 0, length, offset);
          return buf.slice(0, bytesRead);
        } finally {
          await handle.close();
        }
      }
    },
    pwrite: async (path, offset, data) => {
      try { return await base.pwrite(path, offset, data); }
      catch {
        const handle = await fsPromises.open(path, 'r+');
        try {
          await handle.write(data, 0, data.length, offset);
        } finally {
          await handle.close();
        }
      }
    },
  };
}

// ---------------------------------------------------------------------------
// Node RuntimeDriver
// ---------------------------------------------------------------------------

class NodeRuntimeDriver implements RuntimeDriver {
  readonly name = 'node';
  readonly commands: string[] = ['node', 'npm', 'npx'];

  private _kernel: KernelInterface | null = null;
  private _memoryLimit: number;
  private _permissions: Partial<Permissions>;
  private _bindings?: BindingTree;
  private _activeDrivers = new Map<number, NodeExecutionDriver>();
  private _terminatingDrivers = new Map<number, Promise<void>>();
  private _loopbackExemptPorts?: number[];
  private _moduleAccessCwd?: string;
  private _packageRoots?: Array<{ hostPath: string; vmPath: string }>;

  constructor(options?: NodeRuntimeOptions) {
    this._memoryLimit = options?.memoryLimit ?? 128;
    this._permissions = options?.permissions ?? allowAll;
    this._bindings = options?.bindings;
    this._loopbackExemptPorts = options?.loopbackExemptPorts;
    this._moduleAccessCwd = options?.moduleAccessCwd;
    this._packageRoots = options?.packageRoots;
  }

  async init(kernel: KernelInterface): Promise<void> {
    this._kernel = kernel;
  }

  tryResolve(command: string): boolean {
    // Handle .js/.mjs/.cjs file paths as node scripts
    if (/\.[cm]?js$/.test(command)) return true;
    // Handle bare commands resolvable via node_modules/.bin
    if (this._resolveBinCommand(command) !== null) return true;
    return false;
  }

  private _terminateDriver(pid: number, driver: NodeExecutionDriver): Promise<void> {
    const existing = this._terminatingDrivers.get(pid);
    if (existing) {
      return existing;
    }

    const termination = (async () => {
      try {
        await driver.terminate();
      } catch {
        driver.dispose();
      } finally {
        this._activeDrivers.delete(pid);
        this._terminatingDrivers.delete(pid);
      }
    })();

    this._terminatingDrivers.set(pid, termination);
    return termination;
  }

  /**
   * Resolve a bare command name (e.g. 'pi') to a JS entry point via
   * node_modules/.bin on the host filesystem. Returns the VFS path
   * (e.g. '/root/node_modules/@pkg/dist/cli.js') or null if not found.
   *
   * Handles two formats:
   * 1. pnpm shell wrappers: parse `"$basedir/<relative-path>.js"` from the script
   * 2. npm/yarn symlinks or direct JS files: follow to the .js target
   */
  private _resolveBinCommand(command: string): string | null {
    if (!this._moduleAccessCwd) return null;
    const binPath = join(this._moduleAccessCwd, 'node_modules', '.bin', command);
    try {
      const content = readFileSync(binPath, 'utf-8');
      // Direct Node.js script (#!/usr/bin/env node or #!/path/to/node)
      if (/^#!.*\bnode\b/.test(content)) {
        // The .bin file itself is a JS entry — resolve its real path
        // in case it's a symlink (npm/yarn), then map to VFS path
        const realPath = realpathSync(binPath);
        const nmDir = join(this._moduleAccessCwd, 'node_modules');
        if (realPath.startsWith(nmDir)) {
          return '/root/node_modules/' + realPath.slice(nmDir.length + 1);
        }
        // Fallback: use the .bin path itself
        return `/root/node_modules/.bin/${command}`;
      }
      // pnpm/yarn shell wrapper — extract JS path from: "$basedir/<path>.{js,mjs,cjs}"
      const match = content.match(/"\$basedir\/([^"]+\.[cm]?js)"/);
      if (match) {
        // Resolve relative to node_modules/.bin/ on host
        const resolved = resolve(
          join(this._moduleAccessCwd, 'node_modules', '.bin'),
          match[1],
        );
        const nmDir = join(this._moduleAccessCwd, 'node_modules');
        if (resolved.startsWith(nmDir)) {
          return '/root/node_modules/' + resolved.slice(nmDir.length + 1);
        }
      }
    } catch {
      // File doesn't exist or isn't readable
    }
    return null;
  }

  spawn(command: string, args: string[], ctx: ProcessContext): DriverProcess {
    const kernel = this._kernel;
    if (!kernel) throw new Error('Node driver not initialized');

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
    let killedSignal: number | null = null;
    let killExitReported = false;

    const reportKilledExit = (signal: number) => {
      if (killExitReported) return;
      killExitReported = true;
      const exitCode = 128 + signal;
      resolveExit(exitCode);
      proc.onExit?.(exitCode);
    };

    // Stdin plumbing — streaming mode delivers data immediately; batch mode buffers until closeStdin
    let stdinLiveSource: LiveStdinSource | undefined;
    let batchStdinChunks: Uint8Array[] | undefined;
    let batchStdinResolve: ((data: string | undefined) => void) | null = null;
    let batchStdinPromise: Promise<string | undefined> | undefined;

    if (ctx.streamStdin) {
      // Streaming mode: writeStdin delivers data to the running process immediately
      const stdinQueue: Uint8Array[] = [];
      let stdinClosed = false;
      let stdinWaiter: ((value: Uint8Array | null) => void) | null = null;

      stdinLiveSource = {
        read(): Promise<Uint8Array | null> {
          if (stdinQueue.length > 0) {
            return Promise.resolve(stdinQueue.shift()!);
          }
          if (stdinClosed) {
            return Promise.resolve(null);
          }
          return new Promise<Uint8Array | null>((resolve) => {
            stdinWaiter = resolve;
          });
        },
      };

      var streamWriteStdin = (data: Uint8Array) => {
        if (stdinClosed) return;
        if (stdinWaiter) {
          const resolve = stdinWaiter;
          stdinWaiter = null;
          resolve(data);
        } else {
          stdinQueue.push(data);
        }
      };
      var streamCloseStdin = () => {
        if (stdinClosed) return;
        stdinClosed = true;
        if (stdinWaiter) {
          const resolve = stdinWaiter;
          stdinWaiter = null;
          resolve(null);
        }
      };
    } else {
      // Batch mode (default): buffer all stdin data until closeStdin is called
      batchStdinChunks = [];
      batchStdinPromise = new Promise<string | undefined>((resolve) => {
        batchStdinResolve = resolve;
        queueMicrotask(() => {
          if (batchStdinChunks!.length === 0 && batchStdinResolve) {
            batchStdinResolve = null;
            resolve(undefined);
          }
        });
      });
    }

    const proc: DriverProcess = {
      onStdout: null,
      onStderr: null,
      onExit: null,
      writeStdin: ctx.streamStdin
        ? (data: Uint8Array) => streamWriteStdin(data)
        : (data: Uint8Array) => { batchStdinChunks!.push(data); },
      closeStdin: ctx.streamStdin
        ? () => streamCloseStdin()
        : () => {
            if (batchStdinResolve) {
              if (batchStdinChunks!.length === 0) {
                batchStdinResolve(undefined);
              } else {
                const totalLen = batchStdinChunks!.reduce((sum, c) => sum + c.length, 0);
                const merged = new Uint8Array(totalLen);
                let offset = 0;
                for (const chunk of batchStdinChunks!) { merged.set(chunk, offset); offset += chunk.length; }
                batchStdinResolve(new TextDecoder().decode(merged));
              }
              batchStdinResolve = null;
            }
          },
      kill: (signal: number) => {
        if (exitResolved) return;
        const normalizedSignal = signal > 0 ? signal : 15;
        killedSignal = normalizedSignal;
        // Close streaming stdin so pending reads resolve
        if (ctx.streamStdin) {
          streamCloseStdin();
        }
        const driver = this._activeDrivers.get(ctx.pid);
        if (!driver) {
          const terminating = this._terminatingDrivers.get(ctx.pid);
          if (terminating) {
            void terminating.finally(() => {
              reportKilledExit(normalizedSignal);
            });
            return;
          }
          reportKilledExit(normalizedSignal);
          return;
        }
        void this
          ._terminateDriver(ctx.pid, driver)
          .finally(() => {
            reportKilledExit(normalizedSignal);
          });
      },
      wait: () => exitPromise,
    };

    // Launch async — spawn() returns synchronously per RuntimeDriver contract
    this._executeAsync(command, args, ctx, proc, resolveExit, stdinLiveSource, batchStdinPromise, () => killedSignal);

    return proc;
  }

  async dispose(): Promise<void> {
    const terminations = new Set<Promise<void>>();
    for (const [pid, driver] of this._activeDrivers.entries()) {
      terminations.add(this._terminateDriver(pid, driver));
    }
    for (const termination of this._terminatingDrivers.values()) {
      terminations.add(termination);
    }
    await Promise.allSettled([...terminations]);
    this._activeDrivers.clear();
    this._terminatingDrivers.clear();
    this._kernel = null;
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
    liveStdinSource: LiveStdinSource | undefined,
    batchStdinPromise: Promise<string | undefined> | undefined,
    getKilledSignal: () => number | null,
  ): Promise<void> {
    const kernel = this._kernel!;

    try {
      // Resolve the code to execute
      const { code, filePath } = await this._resolveEntry(command, args, kernel);

      if (getKilledSignal() !== null) {
        return;
      }

      // Build kernel-backed system driver
      const commandExecutor = createKernelCommandExecutor(kernel, ctx.pid);
      let filesystem: VirtualFileSystem = createProcessScopedFileSystem(
        createKernelVfsAdapter(kernel.vfs),
        ctx.pid,
      );

      // npm/npx need host filesystem fallback and fs permissions for module resolution
      let permissions: Partial<Permissions> = { ...this._permissions };
      if (command === 'npm' || command === 'npx') {
        filesystem = createHostFallbackVfs(filesystem);
        permissions = { ...permissions, ...allowAllFs };
      }

      // Detect PTY on stdio FDs
      const stdinIsTTY = ctx.stdinIsTTY ?? false;
      const stdoutIsTTY = ctx.stdoutIsTTY ?? false;
      const stderrIsTTY = ctx.stderrIsTTY ?? false;

      // Read PTY dimensions from POSIX env vars set by openShell
      const ptyCols = ctx.env.COLUMNS ? parseInt(ctx.env.COLUMNS, 10) : undefined;
      const ptyRows = ctx.env.LINES ? parseInt(ctx.env.LINES, 10) : undefined;

      const systemDriver = createNodeDriver({
        filesystem,
        moduleAccess: { cwd: this._moduleAccessCwd ?? ctx.cwd, packageRoots: this._packageRoots },
        networkAdapter: kernel.socketTable.hasHostNetworkAdapter()
          ? createDefaultNetworkAdapter({
              initialExemptPorts: this._loopbackExemptPorts,
            })
          : undefined,
        commandExecutor,
        permissions,
        processConfig: {
          cwd: ctx.cwd,
          env: ctx.env,
          argv: [process.execPath, filePath ?? command, ...args],
          stdinIsTTY,
          stdoutIsTTY,
          stderrIsTTY,
          ...(ptyCols !== undefined && !isNaN(ptyCols) ? { cols: ptyCols } : {}),
          ...(ptyRows !== undefined && !isNaN(ptyRows) ? { rows: ptyRows } : {}),
        },
        osConfig: {
          homedir: ctx.env.HOME || '/root',
          tmpdir: ctx.env.TMPDIR || '/tmp',
        },
      });

      // Wire PTY raw mode callback when stdin is a terminal
      const onPtySetRawMode = stdinIsTTY
        ? (mode: boolean) => {
            kernel.tcsetattr(ctx.pid, 0, {
              icanon: !mode,
              echo: !mode,
              isig: !mode,
              icrnl: !mode,
            });
          }
        : undefined;
      // Determine live stdin source: PTY uses kernel fd reads, streaming mode uses the queue
      const effectiveStdinSource: LiveStdinSource | undefined = stdinIsTTY
        ? {
            async read() {
              try {
                const chunk = await kernel.fdRead(ctx.pid, 0, 4096);
                return chunk.length === 0 ? null : chunk;
              } catch {
                return null;
              }
            },
          }
        : liveStdinSource;

      // For batch mode, wait for stdin data before starting the isolate
      let stdinData: string | undefined;
      if (batchStdinPromise) {
        stdinData = await batchStdinPromise;
        if (getKilledSignal() !== null) {
          return;
        }
      }

      // Create a per-process isolate with kernel socket routing
      const executionDriver = new NodeExecutionDriver({
        system: systemDriver,
        runtime: systemDriver.runtime,
        memoryLimit: this._memoryLimit,
        bindings: this._bindings,
        onPtySetRawMode,
        socketTable: kernel.socketTable,
        processTable: kernel.processTable,
        timerTable: kernel.timerTable,
        pid: ctx.pid,
        liveStdinSource: effectiveStdinSource,
      });
      this._activeDrivers.set(ctx.pid, executionDriver);
      const killedSignal = getKilledSignal();
      if (killedSignal !== null) {
        await this._terminateDriver(ctx.pid, executionDriver);
        return;
      }

      // Execute with stdout/stderr capture
      const result = await executionDriver.exec(code, {
        filePath,
        env: ctx.env,
        cwd: ctx.cwd,
        stdin: stdinData,
        onStdio: (event) => {
          const data = new TextEncoder().encode(event.message);
          if (event.channel === 'stdout') {
            ctx.onStdout?.(data);
            proc.onStdout?.(data);
          } else {
            ctx.onStderr?.(data);
            proc.onStderr?.(data);
          }
        },
      });

      // Emit errorMessage as stderr (covers ReferenceError, SyntaxError, throw)
      if (result.errorMessage) {
        const errBytes = new TextEncoder().encode(result.errorMessage + '\n');
        ctx.onStderr?.(errBytes);
        proc.onStderr?.(errBytes);
      }

      // Cleanup isolate and release the shared V8 runtime if this was the last user.
      await this._terminateDriver(ctx.pid, executionDriver);

      resolveExit(result.code);
      proc.onExit?.(result.code);
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : String(err);
      const errBytes = new TextEncoder().encode(`node: ${errMsg}\n`);
      ctx.onStderr?.(errBytes);
      proc.onStderr?.(errBytes);

      // Cleanup on error
      const driver = this._activeDrivers.get(ctx.pid);
      if (driver) {
        await this._terminateDriver(ctx.pid, driver);
      }

      resolveExit(1);
      proc.onExit?.(1);
    }
  }

  // -------------------------------------------------------------------------
  // Entry point resolution
  // -------------------------------------------------------------------------

  /**
   * Resolve the entry code and filePath for a given command.
   * - 'node script.js' → read script from VFS
   * - 'node -e "code"' → inline code
   * - 'npm ...' → host npm CLI entry script
   * - 'npx ...' → host npx CLI entry script
   */
  private async _resolveEntry(
    command: string,
    args: string[],
    kernel: KernelInterface,
  ): Promise<{ code: string; filePath?: string }> {
    if (command === 'npm') {
      const entry = resolveNpmEntry();
      return { code: readFileSync(entry, 'utf-8'), filePath: entry };
    }

    if (command === 'npx') {
      const entry = resolveNpxEntry();
      return { code: readFileSync(entry, 'utf-8'), filePath: entry };
    }

    // .js/.mjs/.cjs file path used as command — treat as `node <path> <args>`
    if (/\.[cm]?js$/.test(command)) {
      return this._resolveNodeArgs([command, ...args], kernel);
    }

    // Bare command — resolve from node_modules/.bin (e.g. 'pi' → '/root/node_modules/.../cli.js')
    const binEntry = this._resolveBinCommand(command);
    if (binEntry) {
      return this._resolveNodeArgs([binEntry, ...args], kernel);
    }

    // 'node' command — parse args to find code/script
    return this._resolveNodeArgs(args, kernel);
  }

  /**
   * Parse Node CLI args to extract the code to execute.
   * Supports: node script.js, node -e "code", node --eval "code",
   * node -p "expr", node --print "expr"
   */
  private async _resolveNodeArgs(
    args: string[],
    kernel: KernelInterface,
  ): Promise<{ code: string; filePath?: string }> {
    for (let i = 0; i < args.length; i++) {
      const arg = args[i];

      // -e / --eval: next arg is code
      if ((arg === '-e' || arg === '--eval') && i + 1 < args.length) {
        return { code: args[i + 1] };
      }

      // -p / --print: wrap in console.log
      if ((arg === '-p' || arg === '--print') && i + 1 < args.length) {
        return { code: `console.log(${args[i + 1]})` };
      }

      // Skip flags
      if (arg.startsWith('-')) continue;

      // First non-flag arg is the script path
      const scriptPath = arg;
      try {
        const content = await kernel.vfs.readTextFile(scriptPath);
        return { code: content, filePath: scriptPath };
      } catch {
        // Fall back to host filesystem for module access paths (/root/node_modules/*)
        if (scriptPath.startsWith('/root/node_modules/')) {
          // Check package roots first (longest prefix match).
          let hostPath: string | null = null;
          if (this._packageRoots) {
            for (const root of this._packageRoots) {
              if (scriptPath === root.vmPath || scriptPath.startsWith(root.vmPath + '/')) {
                const relative = scriptPath.slice(root.vmPath.length + 1);
                hostPath = relative ? join(root.hostPath, relative) : root.hostPath;
                break;
              }
            }
          }
          // Fall back to CWD-based node_modules.
          if (!hostPath && this._moduleAccessCwd) {
            hostPath = join(
              this._moduleAccessCwd,
              'node_modules',
              scriptPath.slice('/root/node_modules/'.length),
            );
          }
          if (hostPath) {
            try {
              const content = readFileSync(hostPath, 'utf-8');
              return { code: content, filePath: scriptPath };
            } catch {
              // Fall through to the error below
            }
          }
        }
        throw new Error(`Cannot find module '${scriptPath}'`);
      }
    }

    // No script or -e flag — read from stdin (not supported yet)
    throw new Error('node: missing script argument (stdin mode not supported)');
  }
}
