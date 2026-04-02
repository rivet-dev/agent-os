/**
 * Worker entry for WasmVM kernel-integrated execution.
 *
 * Runs a single WASM command inside a worker thread. Communicates
 * with the main thread via SharedArrayBuffer RPC for synchronous
 * kernel calls (file I/O, VFS, process spawn) and postMessage for
 * stdout/stderr streaming.
 *
 * proc_spawn is provided as a host_process import so brush-shell
 * pipeline stages route through KernelInterface.spawn() to the
 * correct runtime driver.
 */

import { workerData, parentPort } from 'node:worker_threads';
import { readFile } from 'node:fs/promises';
import { WasiPolyfill, WasiProcExit } from './wasi-polyfill.js';
import { UserManager } from './user.js';
import { FDTable } from './fd-table.js';
import {
  FILETYPE_CHARACTER_DEVICE,
  FILETYPE_REGULAR_FILE,
  FILETYPE_DIRECTORY,
  ERRNO_SUCCESS,
  ERRNO_EACCES,
  ERRNO_ECHILD,
  ERRNO_EINVAL,
  ERRNO_EBADF,
  FDFLAG_NONBLOCK,
  RIGHT_FD_FDSTAT_SET_FLAGS,
  ERRNO_ENOENT,
  RIGHT_FD_READ,
  RIGHT_FD_WRITE,
} from './wasi-constants.js';
import { VfsError } from './wasi-types.js';
import type { WasiVFS, WasiInode, VfsStat, VfsSnapshotEntry } from './wasi-types.js';
import type { WasiFileIO } from './wasi-file-io.js';
import type { WasiProcessIO } from './wasi-process-io.js';
import {
  SIG_IDX_STATE,
  SIG_IDX_ERRNO,
  SIG_IDX_INT_RESULT,
  SIG_IDX_DATA_LEN,
  SIG_IDX_PENDING_SIGNAL,
  SIG_STATE_IDLE,
  SIG_STATE_READY,
  RPC_WAIT_TIMEOUT_MS,
  type WorkerInitData,
  type SyscallRequest,
} from './syscall-rpc.js';
import {
  isWriteBlocked as _isWriteBlocked,
  isSpawnBlocked as _isSpawnBlocked,
  isNetworkBlocked as _isNetworkBlocked,
  isPathInCwd as _isPathInCwd,
  validatePermissionTier,
} from './permission-check.js';
import { normalize } from 'node:path';

const port = parentPort!;
const init = workerData as WorkerInitData;

// Permission tier — validate to default unknown strings to 'isolated'
const permissionTier = validatePermissionTier(init.permissionTier ?? 'read-write');

/** Check if the tier blocks write operations. */
function isWriteBlocked(): boolean {
  return _isWriteBlocked(permissionTier);
}

/** Check if the tier blocks subprocess spawning. */
function isSpawnBlocked(): boolean {
  return _isSpawnBlocked(permissionTier);
}

/** Check if the tier blocks network operations. */
function isNetworkBlocked(): boolean {
  return _isNetworkBlocked(permissionTier);
}

/**
 * Resolve symlinks in path via VFS readlink RPC.
 * Walks each path component and follows symlinks to prevent escape attacks.
 */
function vfsRealpath(inputPath: string): string {
  const segments = inputPath.split('/').filter(Boolean);
  const resolved: string[] = [];
  let depth = 0;
  const MAX_SYMLINK_DEPTH = 40; // POSIX SYMLOOP_MAX

  for (let i = 0; i < segments.length; i++) {
    resolved.push(segments[i]);
    const currentPath = '/' + resolved.join('/');

    // Try readlink directly via RPC (bypasses permission check)
    const res = rpcCall('vfsReadlink', { path: currentPath });
    if (res.errno === 0 && res.data.length > 0) {
      if (++depth > MAX_SYMLINK_DEPTH) return inputPath; // give up
      const target = new TextDecoder().decode(res.data);
      if (target.startsWith('/')) {
        // Absolute symlink — restart from target
        resolved.length = 0;
        resolved.push(...target.split('/').filter(Boolean));
      } else {
        // Relative symlink — replace last component with target
        resolved.pop();
        resolved.push(...target.split('/').filter(Boolean));
      }
      // Normalize away . and .. segments
      const norm = normalize('/' + resolved.join('/')).split('/').filter(Boolean);
      resolved.length = 0;
      resolved.push(...norm);
    }
  }

  return '/' + resolved.join('/') || '/';
}

/** Check if a path is within the cwd subtree (for isolated tier). */
function isPathInCwd(path: string): boolean {
  return _isPathInCwd(path, init.cwd, vfsRealpath);
}

// -------------------------------------------------------------------------
// RPC client — blocks worker thread until main thread responds
// -------------------------------------------------------------------------

const signalArr = new Int32Array(init.signalBuf);
const dataArr = new Uint8Array(init.dataBuf);

// Module-level reference for cooperative signal delivery — set after WASM instantiation
let wasmTrampoline: ((signum: number) => void) | null = null;

function rpcCall(call: string, args: Record<string, unknown>): {
  errno: number;
  intResult: number;
  data: Uint8Array;
} {
  // Reset signal
  Atomics.store(signalArr, SIG_IDX_STATE, SIG_STATE_IDLE);

  // Post request
  const msg: SyscallRequest = { type: 'syscall', call, args };
  port.postMessage(msg);

  // Block until response
  while (true) {
    const result = Atomics.wait(signalArr, SIG_IDX_STATE, SIG_STATE_IDLE, RPC_WAIT_TIMEOUT_MS);
    if (result !== 'timed-out') {
      break;
    }

    // poll(-1) can legally block forever, so keep waiting instead of turning
    // the worker RPC guard timeout into a spurious EIO.
    if (call === 'netPoll' && typeof args.timeout === 'number' && args.timeout < 0) {
      continue;
    }

    return { errno: 76 /* EIO */, intResult: 0, data: new Uint8Array(0) };
  }

  // Read response
  const errno = Atomics.load(signalArr, SIG_IDX_ERRNO);
  const intResult = Atomics.load(signalArr, SIG_IDX_INT_RESULT);
  const dataLen = Atomics.load(signalArr, SIG_IDX_DATA_LEN);
  const data = dataLen > 0 ? dataArr.slice(0, dataLen) : new Uint8Array(0);

  // Cooperative signal delivery — check piggybacked pending signal from driver
  const pendingSig = Atomics.load(signalArr, SIG_IDX_PENDING_SIGNAL);
  if (pendingSig !== 0 && wasmTrampoline) {
    wasmTrampoline(pendingSig);
  }

  // Reset for next call
  Atomics.store(signalArr, SIG_IDX_STATE, SIG_STATE_IDLE);

  return { errno, intResult, data };
}

const FD_POLL_READABLE = 0x1;
const FD_POLL_WRITABLE = 0x2;
const FD_POLL_HANGUP = 0x4;
const FD_POLL_INVALID = 0x8;

// -------------------------------------------------------------------------
// Local FD table — mirrors kernel state for rights checking / routing
// -------------------------------------------------------------------------

// Local FD → kernel FD mapping: the local FD table has a preopen at FD 3
// that the kernel doesn't know about, so opened-file FDs diverge.
const localToKernelFd = new Map<number, number>();

/** Translate a worker-local FD to the kernel FD/socket ID it represents. */
function getKernelFd(localFd: number): number {
  return localToKernelFd.get(localFd) ?? localFd;
}

// Mapping-aware FDTable: updates localToKernelFd on renumber so pipe/redirect
// FDs remain reachable after WASI fd_renumber moves them to stdio positions.
// Also closes the kernel FD of the overwritten target (POSIX renumber semantics).
class KernelFDTable extends FDTable {
  renumber(oldFd: number, newFd: number): number {
    if (oldFd === newFd) {
      return this.has(oldFd) ? ERRNO_SUCCESS : ERRNO_EBADF;
    }

    // Capture mappings before super changes entries
    const sourceMapping = localToKernelFd.get(oldFd);
    const targetKernelFd = localToKernelFd.get(newFd) ?? newFd;

    const result = super.renumber(oldFd, newFd);
    if (result === ERRNO_SUCCESS) {
      // Close kernel FD of overwritten target (mirrors POSIX close-on-renumber)
      rpcCall('fdClose', { fd: targetKernelFd });

      // Move source mapping to target position
      localToKernelFd.delete(oldFd);
      localToKernelFd.delete(newFd);
      if (sourceMapping !== undefined) {
        localToKernelFd.set(newFd, sourceMapping);
      }
    }
    return result;
  }
}

const fdTable = new KernelFDTable();

// -------------------------------------------------------------------------
// Kernel-backed WasiFileIO
// -------------------------------------------------------------------------

function createKernelFileIO(): WasiFileIO {
  return {
    fdRead(fd, maxBytes) {
      const res = rpcCall('fdRead', { fd: getKernelFd(fd), length: maxBytes });
      // Sync local cursor so fd_tell returns consistent values
      if (res.errno === 0 && res.data.length > 0) {
        const entry = fdTable.get(fd);
        if (entry) entry.cursor += BigInt(res.data.length);
      }
      return { errno: res.errno, data: res.data };
    },
    fdWrite(fd, data) {
      // Permission check: read-only/isolated tiers can only write to stdout/stderr
      if (isWriteBlocked() && fd !== 1 && fd !== 2) {
        return { errno: ERRNO_EACCES, written: 0 };
      }
      const res = rpcCall('fdWrite', { fd: getKernelFd(fd), data: Array.from(data) });
      // Sync local cursor so fd_tell returns consistent values
      if (res.errno === 0 && res.intResult > 0) {
        const entry = fdTable.get(fd);
        if (entry) entry.cursor += BigInt(res.intResult);
      }
      return { errno: res.errno, written: res.intResult };
    },
    fdOpen(path, dirflags, oflags, fdflags, rightsBase, rightsInheriting) {
      const createIfMissing = !!(oflags & 0x1); // OFLAG_CREAT
      const wantDirectory = !!(oflags & 0x2); // OFLAG_DIRECTORY

      // Permission check: isolated tier restricts reads to cwd subtree
      if (permissionTier === 'isolated' && !isPathInCwd(path)) {
        return { errno: ERRNO_EACCES, fd: -1, filetype: 0 };
      }

      // Permission check: block write flags for read-only/isolated tiers
      const wantsRead = !!(rightsBase & RIGHT_FD_READ);
      const wantsWrite = !!(rightsBase & RIGHT_FD_WRITE);
      const hasWriteIntent = !!(oflags & 0x1) || !!(oflags & 0x8) || !!(fdflags & 0x1) || wantsWrite;
      if (isWriteBlocked() && hasWriteIntent) {
        return { errno: ERRNO_EACCES, fd: -1, filetype: 0 };
      }

      // Check if the path is actually a directory — some wasi-libc versions
      // omit O_DIRECTORY in oflags when opening directories (e.g., nftw's
      // opendir uses path_open with oflags=0). POSIX allows open(dir, O_RDONLY).
      let isDirectory = wantDirectory;
      if (!isDirectory) {
        const probe = rpcCall('vfsStat', { path });
        if (probe.errno === 0) {
          const raw = JSON.parse(new TextDecoder().decode(probe.data)) as Record<string, unknown>;
          if (raw.type === 'dir') isDirectory = true;
        }
      }

      // Directory opens: verify path exists as directory, return local FD
      // No kernel FD needed — directory ops use VFS RPCs, not kernel fdRead
      if (isDirectory) {
        if (!wantDirectory) {
          // Already stat'd above
        } else {
          const statRes = rpcCall('vfsStat', { path });
          if (statRes.errno !== 0) return { errno: 44 /* ENOENT */, fd: -1, filetype: 0 };
        }

        const localFd = fdTable.open(
          { type: 'preopen', path },
          { filetype: FILETYPE_DIRECTORY, rightsBase, rightsInheriting, fdflags, path },
        );
        return { errno: 0, fd: localFd, filetype: FILETYPE_DIRECTORY };
      }

      // The kernel FD layer only materializes missing paths for O_CREAT/O_EXCL/O_TRUNC
      // via prepareOpenSync. For a plain open on a nonexistent file, reject here so
      // callers observe POSIX/WASI ENOENT instead of receiving an unusable descriptor.
      if (!createIfMissing) {
        const statRes = rpcCall('vfsStat', { path });
        if (statRes.errno !== 0) {
          return { errno: ERRNO_ENOENT, fd: -1, filetype: 0 };
        }
      }

      // Map WASI oflags to POSIX open flags for kernel
      let flags = 0;
      if (oflags & 0x1) flags |= 0o100;   // O_CREAT
      if (oflags & 0x4) flags |= 0o200;   // O_EXCL
      if (oflags & 0x8) flags |= 0o1000;  // O_TRUNC
      if (fdflags & 0x1) flags |= 0o2000; // O_APPEND
      if (wantsRead && wantsWrite) flags |= 2; // O_RDWR
      else if (wantsWrite) flags |= 1;         // O_WRONLY

      const res = rpcCall('fdOpen', { path, flags, mode: 0o666 });
      if (res.errno !== 0) return { errno: res.errno, fd: -1, filetype: 0 };

      const kFd = res.intResult; // kernel FD

      // Mirror in local FDTable for polyfill rights checking
      const localFd = fdTable.open(
        { type: 'vfsFile', ino: 0, path },
        { filetype: FILETYPE_REGULAR_FILE, rightsBase, rightsInheriting, fdflags, path },
      );
      localToKernelFd.set(localFd, kFd);
      return { errno: 0, fd: localFd, filetype: FILETYPE_REGULAR_FILE };
    },
    fdSeek(fd, offset, whence) {
      const res = rpcCall('fdSeek', { fd: getKernelFd(fd), offset: offset.toString(), whence });
      return { errno: res.errno, newOffset: BigInt(res.intResult) };
    },
    fdClose(fd) {
      const entry = fdTable.get(fd);
      const kFd = getKernelFd(fd);
      fdTable.close(fd);
      localToKernelFd.delete(fd);
      const res = entry?.resource.type === 'socket'
        ? rpcCall('netClose', { fd: kFd })
        : rpcCall('fdClose', { fd: kFd });
      return res.errno;
    },
    fdPread(fd, maxBytes, offset) {
      const res = rpcCall('fdPread', { fd: getKernelFd(fd), length: maxBytes, offset: offset.toString() });
      return { errno: res.errno, data: res.data };
    },
    fdPwrite(fd, data, offset) {
      // Permission check: read-only/isolated tiers can only write to stdout/stderr
      if (isWriteBlocked() && fd !== 1 && fd !== 2) {
        return { errno: ERRNO_EACCES, written: 0 };
      }
      const res = rpcCall('fdPwrite', { fd: getKernelFd(fd), data: Array.from(data), offset: offset.toString() });
      return { errno: res.errno, written: res.intResult };
    },
  };
}

// -------------------------------------------------------------------------
// Kernel-backed WasiProcessIO
// -------------------------------------------------------------------------

function createKernelProcessIO(): WasiProcessIO {
  return {
    getArgs() {
      return [init.command, ...init.args];
    },
    getEnviron() {
      return init.env;
    },
    fdFdstatGet(fd) {
      const entry = fdTable.get(fd);
      if (!entry) {
        return { errno: 8 /* EBADF */, filetype: 0, fdflags: 0, rightsBase: 0n, rightsInheriting: 0n };
      }
      return {
        errno: 0,
        filetype: entry.filetype,
        fdflags: entry.fdflags,
        rightsBase: entry.rightsBase,
        rightsInheriting: entry.rightsInheriting,
      };
    },
    fdFdstatSetFlags(fd, flags) {
      const entry = fdTable.get(fd);
      if (!entry) {
        return ERRNO_EBADF;
      }
      if (!(entry.rightsBase & RIGHT_FD_FDSTAT_SET_FLAGS)) {
        return ERRNO_EBADF;
      }

      entry.fdflags = flags;

      if (entry.resource.type === 'socket') {
        const res = rpcCall('netSetNonBlocking', {
          fd: getKernelFd(fd),
          nonBlocking: (flags & FDFLAG_NONBLOCK) !== 0,
        });
        if (res.errno !== 0) {
          return res.errno;
        }
      }

      return ERRNO_SUCCESS;
    },
    procExit(exitCode) {
      // Exit notification handled by WasiProcExit exception path
    },
  };
}

// -------------------------------------------------------------------------
// Kernel-backed VFS proxy — routes through RPC
// -------------------------------------------------------------------------

function createKernelVfs(): WasiVFS {
  const decoder = new TextDecoder();

  // Inode cache for getIno/getInodeByIno — synthesizes inodes from kernel VFS stat
  let nextIno = 1;
  const pathToIno = new Map<string, number>();
  const inoToPath = new Map<number, string>();
  const inoCache = new Map<number, WasiInode>();
  const populatedDirs = new Set<number>();

  function resolveIno(path: string, followSymlinks = true): number | null {
    if (permissionTier === 'isolated' && !isPathInCwd(path)) return null;

    // When following symlinks, use cached inode if available
    if (followSymlinks) {
      const cached = pathToIno.get(path);
      if (cached !== undefined) return cached;
    }

    const rpcName = followSymlinks ? 'vfsStat' : 'vfsLstat';
    const res = rpcCall(rpcName, { path });
    if (res.errno !== 0) return null;

    // RPC response fields: { type, mode, uid, gid, nlink, size, atime, mtime, ctime }
    const raw = JSON.parse(decoder.decode(res.data)) as Record<string, unknown>;
    const ino = nextIno++;
    pathToIno.set(path, ino);
    inoToPath.set(ino, path);

    const nodeType = raw.type as string ?? 'file';
    const isDir = nodeType === 'dir';
    const node: WasiInode = {
      type: nodeType,
      mode: (raw.mode as number) ?? (isDir ? 0o40755 : 0o100644),
      uid: (raw.uid as number) ?? 0,
      gid: (raw.gid as number) ?? 0,
      nlink: (raw.nlink as number) ?? 1,
      size: (raw.size as number) ?? 0,
      atime: (raw.atime as number) ?? Date.now(),
      mtime: (raw.mtime as number) ?? Date.now(),
      ctime: (raw.ctime as number) ?? Date.now(),
    };

    if (isDir) {
      node.entries = new Map();
    }

    inoCache.set(ino, node);
    return ino;
  }

  function refreshCachedInode(ino: number): void {
    const path = inoToPath.get(ino);
    const node = inoCache.get(ino);
    if (!path || !node) return;

    if (permissionTier === 'isolated' && !isPathInCwd(path)) return;

    const res = rpcCall('vfsStat', { path });
    if (res.errno !== 0) return;

    const raw = JSON.parse(decoder.decode(res.data)) as Record<string, unknown>;
    const nodeType = (raw.type as string) ?? 'file';
    node.type = nodeType;
    node.mode = (raw.mode as number) ?? node.mode;
    node.uid = (raw.uid as number) ?? node.uid;
    node.gid = (raw.gid as number) ?? node.gid;
    node.nlink = (raw.nlink as number) ?? node.nlink;
    node.atime = (raw.atime as number) ?? node.atime;
    node.mtime = (raw.mtime as number) ?? node.mtime;
    node.ctime = (raw.ctime as number) ?? node.ctime;
    (node as WasiInode & { size: number }).size = (raw.size as number) ?? node.size;

    if (nodeType === 'dir') {
      node.entries ??= new Map();
    }
  }

  /** Lazy-populate directory entries from kernel VFS readdir. */
  function populateDirEntries(ino: number, node: WasiInode): void {
    if (populatedDirs.has(ino)) return;
    populatedDirs.add(ino);

    const path = inoToPath.get(ino);
    if (!path) return;

    // Isolated tier: skip populating directories outside cwd
    if (permissionTier === 'isolated' && !isPathInCwd(path)) return;

    const res = rpcCall('vfsReaddir', { path });
    if (res.errno !== 0) return;

    const names = JSON.parse(decoder.decode(res.data)) as string[];
    for (const name of names) {
      const childPath = path === '/' ? '/' + name : path + '/' + name;
      const childIno = resolveIno(childPath);
      if (childIno !== null) {
        node.entries!.set(name, childIno);
      }
    }
  }

  return {
    exists(path: string): boolean {
      if (permissionTier === 'isolated' && !isPathInCwd(path)) return false;
      const res = rpcCall('vfsExists', { path });
      return res.errno === 0 && res.intResult === 1;
    },
    mkdir(path: string): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', path);
      const res = rpcCall('vfsMkdir', { path });
      if (res.errno !== 0) throw new VfsError('EACCES', path);
    },
    mkdirp(path: string): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', path);
      const segments = path.split('/').filter(Boolean);
      let current = '';
      for (const seg of segments) {
        current += '/' + seg;
        const exists = rpcCall('vfsExists', { path: current });
        if (exists.errno === 0 && exists.intResult === 0) {
          rpcCall('vfsMkdir', { path: current });
        }
      }
    },
    writeFile(path: string, data: Uint8Array | string): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', path);
      const bytes = typeof data === 'string' ? new TextEncoder().encode(data) : data;
      rpcCall('vfsWriteFile', { path, data: Array.from(bytes) });
    },
    truncate(path: string, length: number): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', path);
      const res = rpcCall('vfsTruncate', { path, length });
      if (res.errno !== 0) throw new VfsError('EINVAL', path);

      const cachedIno = pathToIno.get(path);
      if (cachedIno !== undefined) {
        const node = inoCache.get(cachedIno);
        if (node) {
          const mutableNode = node as WasiInode & { size: number };
          mutableNode.size = length;
          mutableNode.mtime = Date.now();
          mutableNode.ctime = Date.now();
        }
      }
    },
    readFile(path: string): Uint8Array {
      // Isolated tier: restrict reads to cwd subtree
      if (permissionTier === 'isolated' && !isPathInCwd(path)) {
        throw new VfsError('EACCES', path);
      }
      const res = rpcCall('vfsReadFile', { path });
      if (res.errno !== 0) throw new VfsError('ENOENT', path);
      return res.data;
    },
    readdir(path: string): string[] {
      if (permissionTier === 'isolated' && !isPathInCwd(path)) {
        throw new VfsError('EACCES', path);
      }
      const res = rpcCall('vfsReaddir', { path });
      if (res.errno !== 0) throw new VfsError('ENOENT', path);
      return JSON.parse(decoder.decode(res.data));
    },
    stat(path: string): VfsStat {
      if (permissionTier === 'isolated' && !isPathInCwd(path)) {
        throw new VfsError('EACCES', path);
      }
      const res = rpcCall('vfsStat', { path });
      if (res.errno !== 0) throw new VfsError('ENOENT', path);
      return JSON.parse(decoder.decode(res.data));
    },
    lstat(path: string): VfsStat {
      if (permissionTier === 'isolated' && !isPathInCwd(path)) {
        throw new VfsError('EACCES', path);
      }
      const res = rpcCall('vfsLstat', { path });
      if (res.errno !== 0) throw new VfsError('ENOENT', path);
      return JSON.parse(decoder.decode(res.data));
    },
    unlink(path: string): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', path);
      const res = rpcCall('vfsUnlink', { path });
      if (res.errno !== 0) throw new VfsError('ENOENT', path);
    },
    rmdir(path: string): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', path);
      const res = rpcCall('vfsRmdir', { path });
      if (res.errno !== 0) throw new VfsError('ENOENT', path);
    },
    rename(oldPath: string, newPath: string): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', oldPath);
      const res = rpcCall('vfsRename', { oldPath, newPath });
      if (res.errno !== 0) throw new VfsError('ENOENT', oldPath);
    },
    symlink(target: string, linkPath: string): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', linkPath);
      const res = rpcCall('vfsSymlink', { target, linkPath });
      if (res.errno !== 0) throw new VfsError('EEXIST', linkPath);
    },
    readlink(path: string): string {
      if (permissionTier === 'isolated' && !isPathInCwd(path)) {
        throw new VfsError('EACCES', path);
      }
      const res = rpcCall('vfsReadlink', { path });
      if (res.errno !== 0) throw new VfsError('EINVAL', path);
      return decoder.decode(res.data);
    },
    chmod(path: string, mode: number): void {
      if (isWriteBlocked()) throw new VfsError('EACCES', path);
      const res = rpcCall('vfsChmod', { path, mode });
      if (res.errno !== 0) throw new VfsError('EPERM', path);
      // Invalidate cached inode so subsequent stat picks up the new mode
      const cachedIno = pathToIno.get(path);
      if (cachedIno !== undefined) {
        inoCache.delete(cachedIno);
        inoToPath.delete(cachedIno);
        pathToIno.delete(path);
      }
    },
    getIno(path: string, followSymlinks = true): number | null {
      return resolveIno(path, followSymlinks);
    },
    getInodeByIno(ino: number): WasiInode | null {
      const node = inoCache.get(ino);
      if (!node) return null;
      refreshCachedInode(ino);
      // Lazy-populate directory entries from kernel VFS
      if (node.type === 'dir' && node.entries) {
        populateDirEntries(ino, node);
      }
      return node;
    },
    snapshot(): VfsSnapshotEntry[] {
      return [];
    },
  };
}

// -------------------------------------------------------------------------
// Host process imports — proc_spawn, fd_pipe, proc_kill route through kernel
// -------------------------------------------------------------------------

function createHostProcessImports(getMemory: () => WebAssembly.Memory | null) {
  // Track child PIDs for waitpid(-1) — "wait for any child"
  const childPids = new Set<number>();

  return {
    /**
     * proc_spawn routes through KernelInterface.spawn() so brush-shell
     * pipeline stages dispatch to the correct runtime driver.
     *
     * Matches Rust FFI: proc_spawn(argv_ptr, argv_len, envp_ptr, envp_len,
     *   stdin_fd, stdout_fd, stderr_fd, cwd_ptr, cwd_len, ret_pid) -> errno
     */
    proc_spawn(
      argv_ptr: number, argv_len: number,
      envp_ptr: number, envp_len: number,
      stdin_fd: number, stdout_fd: number, stderr_fd: number,
      cwd_ptr: number, cwd_len: number,
      ret_pid_ptr: number,
    ): number {
      // Permission check: only 'full' tier allows subprocess spawning
      if (isSpawnBlocked()) return ERRNO_EACCES;

      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const bytes = new Uint8Array(mem.buffer);
      const decoder = new TextDecoder();

      // Parse null-separated argv buffer — first entry is the command
      const argvRaw = decoder.decode(bytes.slice(argv_ptr, argv_ptr + argv_len));
      const argvParts = argvRaw.split('\0').filter(Boolean);
      const command = argvParts[0] ?? '';
      const args = argvParts.slice(1);

      // Parse null-separated envp buffer (KEY=VALUE\0 pairs)
      const env: Record<string, string> = {};
      if (envp_len > 0) {
        const envpRaw = decoder.decode(bytes.slice(envp_ptr, envp_ptr + envp_len));
        for (const entry of envpRaw.split('\0')) {
          if (!entry) continue;
          const eq = entry.indexOf('=');
          if (eq > 0) env[entry.slice(0, eq)] = entry.slice(eq + 1);
        }
      }

      // Parse cwd — if the caller passed an explicit cwd, use it; otherwise
      // query the kernel for the parent's current working directory so that
      // chdir() changes in the parent are reflected in spawned children.
      let cwd: string;
      if (cwd_len > 0) {
        cwd = decoder.decode(bytes.slice(cwd_ptr, cwd_ptr + cwd_len));
      } else {
        const cwdRes = rpcCall('getcwd', {});
        cwd = cwdRes.data.length > 0
          ? decoder.decode(cwdRes.data)
          : init.cwd;
      }

      // Convert local FDs to kernel FDs for pipe wiring
      const stdinFd = stdin_fd === -1 ? undefined : (localToKernelFd.get(stdin_fd) ?? stdin_fd);
      const stdoutFd = stdout_fd === -1 ? undefined : (localToKernelFd.get(stdout_fd) ?? stdout_fd);
      const stderrFd = stderr_fd === -1 ? undefined : (localToKernelFd.get(stderr_fd) ?? stderr_fd);

      // Route through kernel with FD overrides for pipe wiring
      const res = rpcCall('spawn', {
        command,
        spawnArgs: args,
        env,
        cwd,
        stdinFd,
        stdoutFd,
        stderrFd,
      });

      if (res.errno !== 0) return res.errno;
      const childPid = res.intResult;
      new DataView(mem.buffer).setUint32(ret_pid_ptr, childPid, true);
      childPids.add(childPid);

      // Close pipe FDs used as stdio overrides in the parent (POSIX close-after-fork)
      // Without this, the parent retains a reference to the pipe ends, preventing EOF.
      for (const localFd of [stdin_fd, stdout_fd, stderr_fd]) {
        if (localFd >= 0 && localToKernelFd.has(localFd)) {
          const kFd = localToKernelFd.get(localFd)!;
          fdTable.close(localFd);
          localToKernelFd.delete(localFd);
          rpcCall('fdClose', { fd: kFd });
        }
      }

      return ERRNO_SUCCESS;
    },

    /**
     * proc_waitpid(pid, options, ret_status, ret_pid) -> errno
     * options: 0 = blocking, 1 = WNOHANG
     * ret_pid: writes the actual waited-for PID (relevant for pid=-1)
     */
    proc_waitpid(pid: number, options: number, ret_status_ptr: number, ret_pid_ptr: number): number {
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      // Resolve pid=-1 (wait for any child) to an actual child PID
      let targetPid = pid;
      if (pid < 0) {
        const first = childPids.values().next();
        if (first.done) return ERRNO_ECHILD;
        targetPid = first.value;
      }

      const res = rpcCall('waitpid', { pid: targetPid, options: options || undefined });
      if (res.errno !== 0) return res.errno;

      // WNOHANG returns intResult=-1 when process is still running
      if (res.intResult === -1) {
        const view = new DataView(mem.buffer);
        view.setUint32(ret_status_ptr, 0, true);
        view.setUint32(ret_pid_ptr, 0, true);
        return ERRNO_SUCCESS;
      }

      const view = new DataView(mem.buffer);
      view.setUint32(ret_status_ptr, res.intResult, true);
      view.setUint32(ret_pid_ptr, targetPid, true);

      // Remove from tracked children after successful wait
      childPids.delete(targetPid);

      return ERRNO_SUCCESS;
    },

    /** proc_kill(pid, signal) -> errno — only 'full' tier can send signals */
    proc_kill(pid: number, signal: number): number {
      if (isSpawnBlocked()) return ERRNO_EACCES;
      const res = rpcCall('kill', { pid, signal });
      return res.errno;
    },

    /**
     * fd_pipe(ret_read_fd, ret_write_fd) -> errno
     * Creates a kernel pipe and installs both ends in this process's FD table.
     * Registers pipe FDs in the local FDTable so WASI fd_renumber can find them.
     */
    fd_pipe(ret_read_fd_ptr: number, ret_write_fd_ptr: number): number {
      // Permission check: pipes are only useful with proc_spawn, restrict to 'full' tier
      if (isSpawnBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const res = rpcCall('pipe', {});
      if (res.errno !== 0) return res.errno;

      // Read/write FDs packed in intResult: read in low 16 bits, write in high 16 bits
      const kernelReadFd = res.intResult & 0xFFFF;
      const kernelWriteFd = (res.intResult >>> 16) & 0xFFFF;

      // Register pipe FDs in local table as vfsFile — fd_read/fd_write for
      // vfsFile routes through kernel FileIO bridge, which detects pipe FDs
      const localReadFd = fdTable.open(
        { type: 'vfsFile', ino: 0, path: '' },
        { filetype: FILETYPE_CHARACTER_DEVICE },
      );
      const localWriteFd = fdTable.open(
        { type: 'vfsFile', ino: 0, path: '' },
        { filetype: FILETYPE_CHARACTER_DEVICE },
      );
      localToKernelFd.set(localReadFd, kernelReadFd);
      localToKernelFd.set(localWriteFd, kernelWriteFd);

      const view = new DataView(mem.buffer);
      view.setUint32(ret_read_fd_ptr, localReadFd, true);
      view.setUint32(ret_write_fd_ptr, localWriteFd, true);
      return ERRNO_SUCCESS;
    },

    /**
     * fd_dup(fd, ret_new_fd) -> errno
     * Converts local FD to kernel FD, dups in kernel, registers new local FD.
     */
    fd_dup(fd: number, ret_new_fd_ptr: number): number {
      // Permission check: prevent resource exhaustion from restricted tiers
      if (isSpawnBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const kFd = localToKernelFd.get(fd) ?? fd;
      const res = rpcCall('fdDup', { fd: kFd });
      if (res.errno !== 0) return res.errno;

      const newKernelFd = res.intResult;
      const newLocalFd = fdTable.open(
        { type: 'vfsFile', ino: 0, path: '' },
        { filetype: FILETYPE_CHARACTER_DEVICE },
      );
      localToKernelFd.set(newLocalFd, newKernelFd);

      new DataView(mem.buffer).setUint32(ret_new_fd_ptr, newLocalFd, true);
      return ERRNO_SUCCESS;
    },

    /**
     * fd_dup_min(fd, min_fd, ret_new_fd) -> errno
     * F_DUPFD semantics: duplicate fd to lowest available FD >= min_fd.
     */
    fd_dup_min(fd: number, min_fd: number, ret_new_fd_ptr: number): number {
      if (isSpawnBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const kFd = localToKernelFd.get(fd) ?? fd;
      const res = rpcCall('fdDupMin', { fd: kFd, minFd: min_fd });
      if (res.errno !== 0) return res.errno;

      const newKernelFd = res.intResult;
      const newLocalFd = fdTable.dupMinFd(fd, min_fd);
      if (newLocalFd < 0) return ERRNO_EBADF;
      localToKernelFd.set(newLocalFd, newKernelFd);

      new DataView(mem.buffer).setUint32(ret_new_fd_ptr, newLocalFd, true);
      return ERRNO_SUCCESS;
    },

    /** proc_getpid(ret_pid) -> errno */
    proc_getpid(ret_pid_ptr: number): number {
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      new DataView(mem.buffer).setUint32(ret_pid_ptr, init.pid, true);
      return ERRNO_SUCCESS;
    },

    /** proc_getppid(ret_pid) -> errno */
    proc_getppid(ret_pid_ptr: number): number {
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      new DataView(mem.buffer).setUint32(ret_pid_ptr, init.ppid, true);
      return ERRNO_SUCCESS;
    },

    /**
     * fd_dup2(old_fd, new_fd) -> errno
     * Duplicates old_fd to new_fd. If new_fd is already open, it is closed first.
     */
    fd_dup2(old_fd: number, new_fd: number): number {
      // Permission check: prevent resource exhaustion from restricted tiers
      if (isSpawnBlocked()) return ERRNO_EACCES;

      const kOldFd = localToKernelFd.get(old_fd) ?? old_fd;
      const kNewFd = localToKernelFd.get(new_fd) ?? new_fd;
      const res = rpcCall('fdDup2', { oldFd: kOldFd, newFd: kNewFd });
      if (res.errno !== 0) return res.errno;

      // Update local FD table to reflect the dup2
      const errno = fdTable.dup2(old_fd, new_fd);
      if (errno !== ERRNO_SUCCESS) return errno;

      // Map local new_fd to kNewFd (the kernel fd it now owns after dup2).
      // Using kNewFd (not kOldFd) preserves independent ownership: closing
      // new_fd closes kNewFd without affecting old_fd's kOldFd.
      localToKernelFd.set(new_fd, kNewFd);

      return ERRNO_SUCCESS;
    },

    /** sleep_ms(milliseconds) -> errno — blocks via Atomics.wait */
    sleep_ms(milliseconds: number): number {
      const buf = new Int32Array(new SharedArrayBuffer(4));
      Atomics.wait(buf, 0, 0, milliseconds);
      return ERRNO_SUCCESS;
    },

    /**
     * pty_open(ret_master_fd, ret_write_fd) -> errno
     * Allocates a PTY master/slave pair via the kernel and installs both FDs.
     * The slave FD is passed to proc_spawn as stdin/stdout/stderr for interactive use.
     */
    pty_open(ret_master_fd_ptr: number, ret_slave_fd_ptr: number): number {
      if (isSpawnBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const res = rpcCall('openpty', {});
      if (res.errno !== 0) return res.errno;

      // Master + slave kernel FDs packed: low 16 bits = masterFd, high 16 bits = slaveFd
      const kernelMasterFd = res.intResult & 0xFFFF;
      const kernelSlaveFd = (res.intResult >>> 16) & 0xFFFF;

      // Register PTY FDs in local table (same pattern as fd_pipe)
      const localMasterFd = fdTable.open(
        { type: 'vfsFile', ino: 0, path: '' },
        { filetype: FILETYPE_CHARACTER_DEVICE },
      );
      const localSlaveFd = fdTable.open(
        { type: 'vfsFile', ino: 0, path: '' },
        { filetype: FILETYPE_CHARACTER_DEVICE },
      );
      localToKernelFd.set(localMasterFd, kernelMasterFd);
      localToKernelFd.set(localSlaveFd, kernelSlaveFd);

      const view = new DataView(mem.buffer);
      view.setUint32(ret_master_fd_ptr, localMasterFd, true);
      view.setUint32(ret_slave_fd_ptr, localSlaveFd, true);
      return ERRNO_SUCCESS;
    },

    /**
     * proc_sigaction(signal, action, mask_lo, mask_hi, flags) -> errno
     * Register signal disposition plus sa_mask / sa_flags for cooperative delivery.
     * For action=2, the C sysroot still owns the function pointer; the kernel only
     * needs the POSIX sigaction metadata that affects delivery semantics.
     */
    proc_sigaction(signal: number, action: number, mask_lo: number, mask_hi: number, flags: number): number {
      if (signal < 1 || signal > 64) return ERRNO_EINVAL;
      const res = rpcCall('sigaction', {
        signal,
        action,
        maskLow: mask_lo >>> 0,
        maskHigh: mask_hi >>> 0,
        flags: flags >>> 0,
      });
      return res.errno;
    },
  };
}

// -------------------------------------------------------------------------
// Host net imports — TCP socket operations routed through the kernel
// -------------------------------------------------------------------------

function createHostNetImports(getMemory: () => WebAssembly.Memory | null) {
  function openLocalSocketFd(kernelSocketId: number): number {
    const localFd = fdTable.open(
      { type: 'socket', kernelId: kernelSocketId },
      { filetype: FILETYPE_CHARACTER_DEVICE },
    );
    localToKernelFd.set(localFd, kernelSocketId);
    return localFd;
  }

  return {
    /** net_socket(domain, type, protocol, ret_fd) -> errno */
    net_socket(domain: number, type: number, protocol: number, ret_fd_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const res = rpcCall('netSocket', { domain, type, protocol });
      if (res.errno !== 0) return res.errno;

      const localFd = openLocalSocketFd(res.intResult);
      new DataView(mem.buffer).setUint32(ret_fd_ptr, localFd, true);
      return ERRNO_SUCCESS;
    },

    /** net_connect(fd, addr_ptr, addr_len) -> errno */
    net_connect(fd: number, addr_ptr: number, addr_len: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const addrBytes = new Uint8Array(mem.buffer, addr_ptr, addr_len);
      const addr = new TextDecoder().decode(addrBytes);

      const res = rpcCall('netConnect', { fd: getKernelFd(fd), addr });
      return res.errno;
    },

    /** net_send(fd, buf_ptr, buf_len, flags, ret_sent) -> errno */
    net_send(fd: number, buf_ptr: number, buf_len: number, flags: number, ret_sent_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const sendData = new Uint8Array(mem.buffer).slice(buf_ptr, buf_ptr + buf_len);
      const res = rpcCall('netSend', { fd: getKernelFd(fd), data: Array.from(sendData), flags });
      if (res.errno !== 0) return res.errno;

      new DataView(mem.buffer).setUint32(ret_sent_ptr, res.intResult, true);
      return ERRNO_SUCCESS;
    },

    /** net_recv(fd, buf_ptr, buf_len, flags, ret_received) -> errno */
    net_recv(fd: number, buf_ptr: number, buf_len: number, flags: number, ret_received_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const res = rpcCall('netRecv', { fd: getKernelFd(fd), length: buf_len, flags });
      if (res.errno !== 0) return res.errno;

      // Copy received data into WASM memory
      const dest = new Uint8Array(mem.buffer, buf_ptr, buf_len);
      dest.set(res.data.subarray(0, Math.min(res.data.length, buf_len)));
      new DataView(mem.buffer).setUint32(ret_received_ptr, res.data.length, true);
      return ERRNO_SUCCESS;
    },

    /** net_close(fd) -> errno */
    net_close(fd: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const res = rpcCall('netClose', { fd: getKernelFd(fd) });
      if (res.errno === 0) {
        localToKernelFd.delete(fd);
      }
      return res.errno;
    },

    /** net_tls_connect(fd, hostname_ptr, hostname_len, flags?) -> errno
     *  flags: 0 = verify peer (default), 1 = skip verification (-k) */
    net_tls_connect(fd: number, hostname_ptr: number, hostname_len: number, flags?: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const hostnameBytes = new Uint8Array(mem.buffer, hostname_ptr, hostname_len);
      const hostname = new TextDecoder().decode(hostnameBytes);
      const verifyPeer = (flags ?? 0) === 0;

      const res = rpcCall('netTlsConnect', { fd: getKernelFd(fd), hostname, verifyPeer });
      return res.errno;
    },

    /** net_getaddrinfo(host_ptr, host_len, port_ptr, port_len, ret_addr, ret_addr_len) -> errno */
    net_getaddrinfo(
      host_ptr: number, host_len: number,
      port_ptr: number, port_len: number,
      ret_addr_ptr: number, ret_addr_len_ptr: number,
    ): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const decoder = new TextDecoder();
      const host = decoder.decode(new Uint8Array(mem.buffer, host_ptr, host_len));
      const port = decoder.decode(new Uint8Array(mem.buffer, port_ptr, port_len));

      const res = rpcCall('netGetaddrinfo', { host, port });
      if (res.errno !== 0) return res.errno;

      // Write resolved address data back to WASM memory
      const maxLen = new DataView(mem.buffer).getUint32(ret_addr_len_ptr, true);
      const dataLen = res.data.length;
      if (dataLen > maxLen) return ERRNO_EINVAL;

      const wasmBuf = new Uint8Array(mem.buffer);
      wasmBuf.set(res.data.subarray(0, dataLen), ret_addr_ptr);
      new DataView(mem.buffer).setUint32(ret_addr_len_ptr, dataLen, true);

      return 0;
    },

    /** net_setsockopt(fd, level, optname, optval_ptr, optval_len) -> errno */
    net_setsockopt(fd: number, level: number, optname: number, optval_ptr: number, optval_len: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const optval = new Uint8Array(mem.buffer).slice(optval_ptr, optval_ptr + optval_len);
      const res = rpcCall('netSetsockopt', {
        fd: getKernelFd(fd),
        level,
        optname,
        optval: Array.from(optval),
      });
      return res.errno;
    },

    /** net_getsockopt(fd, level, optname, optval_ptr, optval_len_ptr) -> errno */
    net_getsockopt(fd: number, level: number, optname: number, optval_ptr: number, optval_len_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const view = new DataView(mem.buffer);
      const optvalLen = view.getUint32(optval_len_ptr, true);
      const res = rpcCall('netGetsockopt', {
        fd: getKernelFd(fd),
        level,
        optname,
        optvalLen,
      });
      if (res.errno !== 0) return res.errno;
      if (res.data.length > optvalLen) return ERRNO_EINVAL;

      const wasmBuf = new Uint8Array(mem.buffer);
      wasmBuf.set(res.data, optval_ptr);
      view.setUint32(optval_len_ptr, res.data.length, true);
      return ERRNO_SUCCESS;
    },

    /** net_getsockname(fd, ret_addr, ret_addr_len) -> errno */
    net_getsockname(fd: number, ret_addr_ptr: number, ret_addr_len_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const view = new DataView(mem.buffer);
      const maxAddrLen = view.getUint32(ret_addr_len_ptr, true);
      const res = rpcCall('kernelSocketGetLocalAddr', { fd: getKernelFd(fd) });
      if (res.errno !== 0) return res.errno;
      if (res.data.length > maxAddrLen) return ERRNO_EINVAL;

      const wasmBuf = new Uint8Array(mem.buffer);
      wasmBuf.set(res.data, ret_addr_ptr);
      view.setUint32(ret_addr_len_ptr, res.data.length, true);
      return ERRNO_SUCCESS;
    },

    /** net_getpeername(fd, ret_addr, ret_addr_len) -> errno */
    net_getpeername(fd: number, ret_addr_ptr: number, ret_addr_len_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const view = new DataView(mem.buffer);
      const maxAddrLen = view.getUint32(ret_addr_len_ptr, true);
      const res = rpcCall('kernelSocketGetRemoteAddr', { fd: getKernelFd(fd) });
      if (res.errno !== 0) return res.errno;
      if (res.data.length > maxAddrLen) return ERRNO_EINVAL;

      const wasmBuf = new Uint8Array(mem.buffer);
      wasmBuf.set(res.data, ret_addr_ptr);
      view.setUint32(ret_addr_len_ptr, res.data.length, true);
      return ERRNO_SUCCESS;
    },

    /** net_bind(fd, addr_ptr, addr_len) -> errno */
    net_bind(fd: number, addr_ptr: number, addr_len: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const addrBytes = new Uint8Array(mem.buffer, addr_ptr, addr_len);
      const addr = new TextDecoder().decode(addrBytes);

      const res = rpcCall('netBind', { fd: getKernelFd(fd), addr });
      return res.errno;
    },

    /** net_listen(fd, backlog) -> errno */
    net_listen(fd: number, backlog: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;

      const res = rpcCall('netListen', { fd: getKernelFd(fd), backlog });
      return res.errno;
    },

    /** net_accept(fd, ret_fd, ret_addr, ret_addr_len) -> errno */
    net_accept(fd: number, ret_fd_ptr: number, ret_addr_ptr: number, ret_addr_len_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const res = rpcCall('netAccept', { fd: getKernelFd(fd) });
      if (res.errno !== 0) return res.errno;

      const view = new DataView(mem.buffer);
      const newFd = openLocalSocketFd(res.intResult);
      view.setUint32(ret_fd_ptr, newFd, true);

      // res.data contains the remote address string as UTF-8 bytes
      const maxAddrLen = view.getUint32(ret_addr_len_ptr, true);
      const addrLen = Math.min(res.data.length, maxAddrLen);
      const wasmBuf = new Uint8Array(mem.buffer);
      wasmBuf.set(res.data.subarray(0, addrLen), ret_addr_ptr);
      view.setUint32(ret_addr_len_ptr, addrLen, true);

      return ERRNO_SUCCESS;
    },

    /** net_sendto(fd, buf_ptr, buf_len, flags, addr_ptr, addr_len, ret_sent) -> errno */
    net_sendto(fd: number, buf_ptr: number, buf_len: number, flags: number, addr_ptr: number, addr_len: number, ret_sent_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const sendData = new Uint8Array(mem.buffer).slice(buf_ptr, buf_ptr + buf_len);
      const addrBytes = new Uint8Array(mem.buffer, addr_ptr, addr_len);
      const addr = new TextDecoder().decode(addrBytes);

      const res = rpcCall('netSendTo', { fd: getKernelFd(fd), data: Array.from(sendData), flags, addr });
      if (res.errno !== 0) return res.errno;

      new DataView(mem.buffer).setUint32(ret_sent_ptr, res.intResult, true);
      return ERRNO_SUCCESS;
    },

    /** net_recvfrom(fd, buf_ptr, buf_len, flags, ret_received, ret_addr, ret_addr_len) -> errno */
    net_recvfrom(fd: number, buf_ptr: number, buf_len: number, flags: number, ret_received_ptr: number, ret_addr_ptr: number, ret_addr_len_ptr: number): number {
      if (isNetworkBlocked()) return ERRNO_EACCES;
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      const res = rpcCall('netRecvFrom', { fd: getKernelFd(fd), length: buf_len, flags });
      if (res.errno !== 0) return res.errno;

      // intResult = received data length; data buffer = [data | addr bytes]
      const dataLen = res.intResult;
      const dest = new Uint8Array(mem.buffer, buf_ptr, buf_len);
      dest.set(res.data.subarray(0, Math.min(dataLen, buf_len)));
      new DataView(mem.buffer).setUint32(ret_received_ptr, dataLen, true);

      // Source address bytes follow data in the buffer
      const view = new DataView(mem.buffer);
      const maxAddrLen = view.getUint32(ret_addr_len_ptr, true);
      const addrBytes = res.data.subarray(dataLen);
      const addrLen = Math.min(addrBytes.length, maxAddrLen);
      const wasmBuf = new Uint8Array(mem.buffer);
      wasmBuf.set(addrBytes.subarray(0, addrLen), ret_addr_ptr);
      view.setUint32(ret_addr_len_ptr, addrLen, true);

      return ERRNO_SUCCESS;
    },

    /** net_poll(fds_ptr, nfds, timeout_ms, ret_ready) -> errno */
    net_poll(fds_ptr: number, nfds: number, timeout_ms: number, ret_ready_ptr: number): number {
      // No permission gate — poll() is a generic FD operation (pipes, files, sockets).
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;

      // Read pollfd entries from WASM memory: each is 8 bytes (fd:i32, events:i16, revents:i16)
      // Translate local FDs to kernel FDs so the driver can look up pipes/sockets
      const view = new DataView(mem.buffer);
      const fds: Array<{ fd: number; events: number }> = [];
      for (let i = 0; i < nfds; i++) {
        const base = fds_ptr + i * 8;
        const localFd = view.getInt32(base, true);
        const events = view.getInt16(base + 4, true);
        fds.push({ fd: getKernelFd(localFd), events });
      }

      const res = rpcCall('netPoll', { fds, timeout: timeout_ms });
      if (res.errno !== 0) return res.errno;

      // Parse revents from response data (JSON array)
      const reventsJson = new TextDecoder().decode(res.data.subarray(0, res.data.length));
      const revents: number[] = JSON.parse(reventsJson);

      // Write revents back into WASM memory pollfd structs
      for (let i = 0; i < nfds && i < revents.length; i++) {
        const base = fds_ptr + i * 8;
        view.setInt16(base + 6, revents[i], true); // revents field offset = 6
      }

      view.setUint32(ret_ready_ptr, res.intResult, true);
      return ERRNO_SUCCESS;
    },
  };
}

// -------------------------------------------------------------------------
// Host filesystem imports — provides POSIX mode bridge for Rust std
// -------------------------------------------------------------------------

function createHostFsImports(getMemory: () => WebAssembly.Memory | null) {
  const decoder = new TextDecoder();
  return {
    /** Return POSIX mode (including type bits) for a path. 0 on error. */
    path_mode(pathPtr: number, pathLen: number, followSymlinks: number): number {
      const mem = getMemory();
      if (!mem) return 0;
      const path = decoder.decode(new Uint8Array(mem.buffer, pathPtr, pathLen));
      const rpcName = followSymlinks ? 'vfsStat' : 'vfsLstat';
      const res = rpcCall(rpcName, { path });
      if (res.errno !== 0) return 0;
      const raw = JSON.parse(decoder.decode(res.data));
      return (raw.mode as number) ?? 0;
    },
    /** Return POSIX mode for an open fd. 0 on error. */
    fd_mode(fd: number): number {
      const entry = fdTable.get(fd);
      if (!entry || !entry.path) return 0;
      const res = rpcCall('vfsStat', { path: entry.path });
      if (res.errno !== 0) return 0;
      const raw = JSON.parse(decoder.decode(res.data));
      return (raw.mode as number) ?? 0;
    },
    /** chmod(path_ptr, path_len, mode) -> errno */
    chmod(pathPtr: number, pathLen: number, mode: number): number {
      const mem = getMemory();
      if (!mem) return ERRNO_EINVAL;
      const path = decoder.decode(new Uint8Array(mem.buffer, pathPtr, pathLen));
      const res = rpcCall('vfsChmod', { path, mode });
      return res.errno;
    },
  };
}

// -------------------------------------------------------------------------
// Main execution
// -------------------------------------------------------------------------

async function main(): Promise<void> {
  let wasmMemory: WebAssembly.Memory | null = null;
  const getMemory = () => wasmMemory;

  const fileIO = createKernelFileIO();
  const processIO = createKernelProcessIO();
  const vfs = createKernelVfs();

  const polyfill = new WasiPolyfill(fdTable, vfs, {
    fileIO,
    processIO,
    args: [init.command, ...init.args],
    env: init.env,
  });

  // Route stdin through kernel pipe when piped
  if (init.stdinFd !== undefined) {
    polyfill.setStdinReader((buf, offset, length) => {
      while (true) {
        const res = rpcCall('fdRead', { fd: 0, length });
        if (res.errno !== 0) return 0;
        if (res.data.length > 0) {
          const n = Math.min(res.data.length, length);
          buf.set(res.data.subarray(0, n), offset);
          return n;
        }

        const poll = rpcCall('fdPoll', { fd: 0 });
        if (poll.errno !== 0 || (poll.intResult & (FD_POLL_HANGUP | FD_POLL_INVALID)) !== 0) {
          return 0;
        }

        rpcCall('fdPollWait', { fd: 0, timeout: -1 });
      }
    });
  }

  // Stream stdout/stderr — route through kernel pipe when FD is overridden,
  // otherwise stream to main thread via postMessage
  if (init.stdoutFd !== undefined && init.stdoutFd !== 1) {
    // Stdout is piped — route writes through kernel fdWrite on FD 1
    polyfill.setStdoutWriter((buf, offset, length) => {
      const data = buf.slice(offset, offset + length);
      rpcCall('fdWrite', { fd: 1, data: Array.from(data) });
      return length;
    });
  } else {
    polyfill.setStdoutWriter((buf, offset, length) => {
      port.postMessage({ type: 'stdout', data: buf.slice(offset, offset + length) });
      return length;
    });
  }
  if (init.stderrFd !== undefined && init.stderrFd !== 2) {
    // Stderr is piped — route writes through kernel fdWrite on FD 2
    polyfill.setStderrWriter((buf, offset, length) => {
      const data = buf.slice(offset, offset + length);
      rpcCall('fdWrite', { fd: 2, data: Array.from(data) });
      return length;
    });
  } else {
    polyfill.setStderrWriter((buf, offset, length) => {
      port.postMessage({ type: 'stderr', data: buf.slice(offset, offset + length) });
      return length;
    });
  }

  const userManager = new UserManager({
    getMemory,
    fdTable,
    ttyFds: init.ttyFds ? new Set(init.ttyFds) : false,
  });

  // Check for pending signals while poll_oneoff sleeps inside the WASI polyfill.
  polyfill.setSleepHook(() => {
    rpcCall('getpid', { pid: init.pid });
  });

  const hostProcess = createHostProcessImports(getMemory);
  const hostNet = createHostNetImports(getMemory);
  const hostFs = createHostFsImports(getMemory);

  try {
    // Use pre-compiled module from main thread if available, otherwise compile from disk
    const wasmModule = init.wasmModule
      ?? await WebAssembly.compile(await readFile(init.wasmBinaryPath));

    const imports: WebAssembly.Imports = {
      wasi_snapshot_preview1: polyfill.getImports() as WebAssembly.ModuleImports,
      host_user: userManager.getImports() as unknown as WebAssembly.ModuleImports,
      host_process: hostProcess as unknown as WebAssembly.ModuleImports,
      host_net: hostNet as unknown as WebAssembly.ModuleImports,
      host_fs: hostFs as unknown as WebAssembly.ModuleImports,
    };

    const instance = await WebAssembly.instantiate(wasmModule, imports);
    wasmMemory = instance.exports.memory as WebAssembly.Memory;
    polyfill.setMemory(wasmMemory);

    // Wire cooperative signal delivery trampoline (if the WASM binary exports it)
    const trampoline = instance.exports.__wasi_signal_trampoline as ((signum: number) => void) | undefined;
    if (trampoline) wasmTrampoline = trampoline;

    // Run the command
    const start = instance.exports._start as () => void;
    start();

    // Normal exit — flush collected output, close piped FDs for EOF
    flushOutput(polyfill);
    closePipedFds();
    port.postMessage({ type: 'exit', code: 0 });
  } catch (err) {
    if (err instanceof WasiProcExit) {
      flushOutput(polyfill);
      closePipedFds();
      port.postMessage({ type: 'exit', code: err.exitCode });
    } else {
      const errMsg = err instanceof Error ? err.message : String(err);
      port.postMessage({ type: 'stderr', data: new TextEncoder().encode(errMsg + '\n') });
      closePipedFds();
      port.postMessage({ type: 'exit', code: 1 });
    }
  }
}

/** Close piped stdio FDs so readers get EOF. */
function closePipedFds(): void {
  if (init.stdoutFd !== undefined && init.stdoutFd !== 1) {
    rpcCall('fdClose', { fd: 1 });
  }
  if (init.stderrFd !== undefined && init.stderrFd !== 2) {
    rpcCall('fdClose', { fd: 2 });
  }
}

/** Flush any remaining collected output (not caught by streaming writers). */
function flushOutput(polyfill: WasiPolyfill): void {
  const stdout = polyfill.stdout;
  if (stdout.length > 0) port.postMessage({ type: 'stdout', data: stdout });
  const stderr = polyfill.stderr;
  if (stderr.length > 0) port.postMessage({ type: 'stderr', data: stderr });
}

main().catch((err) => {
  const errMsg = err instanceof Error ? err.message : String(err);
  port.postMessage({ type: 'stderr', data: new TextEncoder().encode(errMsg + '\n') });
  port.postMessage({ type: 'exit', code: 1 });
});
