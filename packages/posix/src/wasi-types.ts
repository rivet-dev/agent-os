/**
 * WASI type definitions and interfaces.
 *
 * Defines the contracts that the WASI polyfill depends on.
 * Concrete implementations are provided by the kernel (production)
 * or test helpers (testing).
 */

import type { WasiFiletype } from './wasi-constants.js';
export type { WasiFiletype } from './wasi-constants.js';

// ---------------------------------------------------------------------------
// VFS error types
// ---------------------------------------------------------------------------

/** VFS error codes matching POSIX errno names. */
export type VfsErrorCode = 'ENOENT' | 'EEXIST' | 'ENOTDIR' | 'EISDIR' | 'ENOTEMPTY' | 'EACCES' | 'EBADF' | 'EINVAL' | 'EPERM';

/**
 * Structured error for VFS operations.
 * Carries a machine-readable `code` so callers can map to errno without string matching.
 */
export class VfsError extends Error {
  readonly code: VfsErrorCode;

  constructor(code: VfsErrorCode, message: string) {
    super(`${code}: ${message}`);
    this.code = code;
    this.name = 'VfsError';
  }
}

// ---------------------------------------------------------------------------
// VFS inode and stat types
// ---------------------------------------------------------------------------

/** Stat result for a filesystem object. */
export interface VfsStat {
  ino: number;
  type: string;
  mode: number;
  uid: number;
  gid: number;
  nlink: number;
  size: number;
  atime: number;
  mtime: number;
  ctime: number;
}

/** Snapshot entry used for serializing/deserializing VFS state. */
export interface VfsSnapshotEntry {
  type: string;
  path: string;
  data?: Uint8Array;
  mode?: number;
  target?: string;
}

/**
 * Inode-like object as returned by WasiVFS.getInodeByIno().
 * Provides direct access to file/dir/symlink/dev data for WASI syscalls
 * that need synchronous low-level access (filestat_set_times, readdir, etc.).
 */
export interface WasiInode {
  type: string;
  mode: number;
  uid: number;
  gid: number;
  nlink: number;
  atime: number;
  mtime: number;
  ctime: number;
  /** File data (type === 'file'). */
  data?: Uint8Array;
  /** Directory entries: name → ino (type === 'dir'). */
  entries?: Map<string, number>;
  /** Symlink target (type === 'symlink'). */
  target?: string;
  /** Device type (type === 'dev'). */
  devType?: string;
  /** Computed size. */
  readonly size: number;
}

// ---------------------------------------------------------------------------
// Synchronous VFS interface (for WASI polyfill)
// ---------------------------------------------------------------------------

/**
 * Synchronous VFS operations needed by the WASI polyfill.
 *
 * Unlike the kernel's async VirtualFileSystem, this interface is synchronous
 * because WASI syscalls must return immediately from the WASM import call.
 * In kernel mode, implementations use SharedArrayBuffer + Atomics to
 * bridge async kernel VFS calls. In test mode, an in-memory implementation
 * provides all operations synchronously.
 */
export interface WasiVFS {
  exists(path: string): boolean;
  mkdir(path: string): void;
  mkdirp(path: string): void;
  writeFile(path: string, content: Uint8Array | string): void;
  truncate(path: string, length: number): void;
  readFile(path: string): Uint8Array;
  readdir(path: string): string[];
  stat(path: string): VfsStat;
  lstat(path: string): VfsStat;
  unlink(path: string): void;
  rmdir(path: string): void;
  rename(oldPath: string, newPath: string): void;
  symlink(target: string, linkPath: string): void;
  readlink(path: string): string;
  chmod(path: string, mode: number): void;
  getIno(path: string, followSymlinks?: boolean): number | null;
  getInodeByIno(ino: number): WasiInode | null;
  snapshot(): VfsSnapshotEntry[];
}

// ---------------------------------------------------------------------------
// FD resource types (discriminated union)
// ---------------------------------------------------------------------------

export interface StdioResource {
  type: 'stdio';
  name: 'stdin' | 'stdout' | 'stderr';
}

export interface VfsFileResource {
  type: 'vfsFile';
  ino: number;
  path: string;
}

export interface PreopenResource {
  type: 'preopen';
  path: string;
}

export interface PipeBuffer {
  buffer: Uint8Array;
  readOffset: number;
  writeOffset: number;
  _readerId?: number;
}

export interface PipeResource {
  type: 'pipe';
  pipe: PipeBuffer;
  end: 'read' | 'write';
}

export interface SocketResource {
  type: 'socket';
  kernelId: number;
}

export type FDResource = StdioResource | VfsFileResource | PreopenResource | PipeResource | SocketResource;

// ---------------------------------------------------------------------------
// FD table types
// ---------------------------------------------------------------------------

/**
 * Represents an open file description (distinct from a file descriptor).
 * Multiple FDs can share the same FileDescription via dup()/dup2(),
 * causing them to share the cursor position — per POSIX semantics.
 */
export class FileDescription {
  inode: number;
  cursor: bigint;
  flags: number;
  refCount: number;

  constructor(inode: number, flags: number) {
    this.inode = inode;
    this.cursor = 0n;
    this.flags = flags;
    this.refCount = 1;
  }
}

export interface FDOpenOptions {
  filetype?: WasiFiletype;
  rightsBase?: bigint;
  rightsInheriting?: bigint;
  fdflags?: number;
  path?: string;
}

/**
 * An entry in the file descriptor table.
 */
export class FDEntry {
  resource: FDResource;
  filetype: WasiFiletype;
  rightsBase: bigint;
  rightsInheriting: bigint;
  fdflags: number;
  fileDescription: FileDescription;
  path: string | null;

  /** Convenience accessor — reads/writes the shared FileDescription cursor. */
  get cursor(): bigint {
    return this.fileDescription.cursor;
  }
  set cursor(value: bigint) {
    this.fileDescription.cursor = value;
  }

  constructor(
    resource: FDResource,
    filetype: WasiFiletype,
    rightsBase: bigint,
    rightsInheriting: bigint,
    fdflags: number,
    path?: string,
    fileDescription?: FileDescription,
  ) {
    this.resource = resource;
    this.filetype = filetype;
    this.rightsBase = rightsBase;
    this.rightsInheriting = rightsInheriting;
    this.fdflags = fdflags;
    this.fileDescription = fileDescription ?? new FileDescription(0, fdflags);
    this.path = path ?? null;
  }
}

// ---------------------------------------------------------------------------
// WasiFDTable interface
// ---------------------------------------------------------------------------

/**
 * WASI file descriptor table interface.
 *
 * Manages open file descriptors for a WASI process. In kernel mode,
 * implementations wrap the kernel's ProcessFDTable with WASI-specific
 * metadata (rights, preopens, resource types).
 */
export interface WasiFDTable {
  open(resource: FDResource, options?: FDOpenOptions): number;
  close(fd: number): number;
  get(fd: number): FDEntry | null;
  dup(fd: number): number;
  dup2(oldFd: number, newFd: number): number;
  has(fd: number): boolean;
  renumber(oldFd: number, newFd: number): number;
  readonly size: number;
}
