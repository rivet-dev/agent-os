/**
 * Test helper: standalone bridge implementations for WasiFileIO and WasiProcessIO.
 *
 * These are the original standalone implementations extracted from the source
 * modules (which now export only interfaces). Tests use these bridges to
 * exercise WASI polyfill behavior against in-memory VFS + FDTable without
 * requiring the kernel.
 */

import type { WasiFileIO } from '../../src/wasi-file-io.ts';
import type { WasiProcessIO } from '../../src/wasi-process-io.ts';
import type { WasiFDTable, WasiVFS, WasiInode, FDOpenOptions } from '../../src/wasi-types.ts';
import { VfsError } from '../../src/wasi-types.ts';
import type { VfsErrorCode } from '../../src/wasi-types.ts';
import type { WasiFiletype } from '../../src/wasi-constants.ts';
import {
  FILETYPE_REGULAR_FILE,
  FILETYPE_DIRECTORY,
  FILETYPE_CHARACTER_DEVICE,
  FILETYPE_SYMBOLIC_LINK,
  FDFLAG_APPEND,
  ERRNO_SUCCESS,
  ERRNO_EBADF,
} from '../../src/wasi-constants.ts';

// ---------------------------------------------------------------------------
// WASI errno codes used by the file I/O bridge
// ---------------------------------------------------------------------------
const ERRNO_ESPIPE = 70;
const ERRNO_EISDIR = 31;
const ERRNO_ENOENT = 44;
const ERRNO_EEXIST = 20;
const ERRNO_ENOTDIR = 54;
const ERRNO_EINVAL = 28;
const ERRNO_EIO = 29;

// WASI seek whence
const WHENCE_SET = 0;
const WHENCE_CUR = 1;
const WHENCE_END = 2;

// WASI open flags
const OFLAG_CREAT = 1;
const OFLAG_DIRECTORY = 2;
const OFLAG_EXCL = 4;
const OFLAG_TRUNC = 8;

// WASI lookup flags
const LOOKUP_SYMLINK_FOLLOW = 1;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const ERRNO_MAP: Record<VfsErrorCode, number> = {
  ENOENT: 44, EEXIST: 20, ENOTDIR: 54, EISDIR: 31,
  ENOTEMPTY: 55, EACCES: 2, EBADF: 8, EINVAL: 28, EPERM: 63,
};

function vfsErrorToErrno(e: unknown): number {
  if (e instanceof VfsError) return ERRNO_MAP[e.code] ?? ERRNO_EIO;
  return ERRNO_EIO;
}

function inodeTypeToFiletype(type: string): WasiFiletype {
  switch (type) {
    case 'file': return FILETYPE_REGULAR_FILE;
    case 'dir': return FILETYPE_DIRECTORY;
    case 'symlink': return FILETYPE_SYMBOLIC_LINK;
    case 'dev': return FILETYPE_CHARACTER_DEVICE;
    default: return 0 as WasiFiletype;
  }
}

// ---------------------------------------------------------------------------
// Standalone file I/O bridge
// ---------------------------------------------------------------------------

/**
 * Create a standalone file I/O bridge that wraps WasiVFS + WasiFDTable.
 * Moves vfsFile read/write/seek/open/close logic out of the polyfill.
 */
export function createStandaloneFileIO(fdTable: WasiFDTable, vfs: WasiVFS): WasiFileIO {
  return {
    fdRead(fd, maxBytes) {
      const entry = fdTable.get(fd);
      if (!entry) return { errno: ERRNO_EBADF, data: new Uint8Array(0) };
      if (entry.resource.type !== 'vfsFile') return { errno: ERRNO_EBADF, data: new Uint8Array(0) };

      const node = vfs.getInodeByIno(entry.resource.ino);
      if (!node) return { errno: ERRNO_EBADF, data: new Uint8Array(0) };
      if (node.type === 'dir') return { errno: ERRNO_EISDIR, data: new Uint8Array(0) };
      if (node.type === 'dev') return { errno: ERRNO_SUCCESS, data: new Uint8Array(0) };
      if (node.type !== 'file') return { errno: ERRNO_EBADF, data: new Uint8Array(0) };

      const pos = Number(entry.cursor);
      const data = node.data!;
      const available = data.length - pos;
      if (available <= 0) return { errno: ERRNO_SUCCESS, data: new Uint8Array(0) };

      const n = Math.min(maxBytes, available);
      const result = data.subarray(pos, pos + n);
      entry.cursor = BigInt(pos + n);
      node.atime = Date.now();
      return { errno: ERRNO_SUCCESS, data: result };
    },

    fdWrite(fd, writeData) {
      const entry = fdTable.get(fd);
      if (!entry) return { errno: ERRNO_EBADF, written: 0 };
      if (entry.resource.type !== 'vfsFile') return { errno: ERRNO_EBADF, written: 0 };

      const node = vfs.getInodeByIno(entry.resource.ino);
      if (!node) return { errno: ERRNO_EBADF, written: 0 };
      if (node.type === 'dir') return { errno: ERRNO_EISDIR, written: 0 };
      if (node.type === 'dev') return { errno: ERRNO_SUCCESS, written: writeData.length };
      if (node.type !== 'file') return { errno: ERRNO_EBADF, written: 0 };

      const pos = (entry.fdflags & FDFLAG_APPEND) ? node.data!.length : Number(entry.cursor);
      const endPos = pos + writeData.length;

      if (endPos > node.data!.length) {
        const newData = new Uint8Array(endPos);
        newData.set(node.data!);
        node.data = newData;
      }

      node.data!.set(writeData, pos);
      entry.cursor = BigInt(endPos);
      node.mtime = Date.now();
      return { errno: ERRNO_SUCCESS, written: writeData.length };
    },

    fdOpen(path, dirflags, oflags, fdflags, rightsBase, rightsInheriting) {
      const followSymlinks = !!(dirflags & LOOKUP_SYMLINK_FOLLOW);
      let ino = vfs.getIno(path, followSymlinks);

      if (ino === null) {
        if (!(oflags & OFLAG_CREAT)) return { errno: ERRNO_ENOENT, fd: -1, filetype: 0 };
        try {
          vfs.writeFile(path, new Uint8Array(0));
        } catch (e) {
          return { errno: vfsErrorToErrno(e), fd: -1, filetype: 0 };
        }
        ino = vfs.getIno(path, true);
        if (ino === null) return { errno: ERRNO_ENOENT, fd: -1, filetype: 0 };
      } else {
        if ((oflags & OFLAG_CREAT) && (oflags & OFLAG_EXCL)) {
          return { errno: ERRNO_EEXIST, fd: -1, filetype: 0 };
        }
      }

      const node = vfs.getInodeByIno(ino);
      if (!node) return { errno: ERRNO_ENOENT, fd: -1, filetype: 0 };

      if ((oflags & OFLAG_DIRECTORY) && node.type !== 'dir') {
        return { errno: ERRNO_ENOTDIR, fd: -1, filetype: 0 };
      }

      if ((oflags & OFLAG_TRUNC) && node.type === 'file') {
        node.data = new Uint8Array(0);
        node.mtime = Date.now();
      }

      const filetype = inodeTypeToFiletype(node.type);
      const fd = fdTable.open(
        { type: 'vfsFile', ino, path },
        { filetype, rightsBase, rightsInheriting, fdflags, path },
      );

      return { errno: ERRNO_SUCCESS, fd, filetype };
    },

    fdSeek(fd, offset, whence) {
      const entry = fdTable.get(fd);
      if (!entry) return { errno: ERRNO_EBADF, newOffset: 0n };
      if (entry.filetype !== FILETYPE_REGULAR_FILE) return { errno: ERRNO_ESPIPE, newOffset: 0n };

      let newPos: bigint;
      switch (whence) {
        case WHENCE_SET:
          newPos = offset;
          break;
        case WHENCE_CUR:
          newPos = entry.cursor + offset;
          break;
        case WHENCE_END: {
          if (!entry.resource || entry.resource.type !== 'vfsFile') return { errno: ERRNO_EINVAL, newOffset: 0n };
          const node = vfs.getInodeByIno(entry.resource.ino);
          if (!node || node.type !== 'file') return { errno: ERRNO_EINVAL, newOffset: 0n };
          newPos = BigInt(node.data!.length) + offset;
          break;
        }
        default:
          return { errno: ERRNO_EINVAL, newOffset: 0n };
      }

      if (newPos < 0n) return { errno: ERRNO_EINVAL, newOffset: 0n };

      entry.cursor = newPos;
      return { errno: ERRNO_SUCCESS, newOffset: newPos };
    },

    fdClose(fd) {
      return fdTable.close(fd);
    },

    fdPread(fd, maxBytes, offset) {
      const entry = fdTable.get(fd);
      if (!entry) return { errno: ERRNO_EBADF, data: new Uint8Array(0) };
      if (entry.resource.type !== 'vfsFile') return { errno: ERRNO_EBADF, data: new Uint8Array(0) };

      const node = vfs.getInodeByIno(entry.resource.ino);
      if (!node || node.type !== 'file') return { errno: ERRNO_EBADF, data: new Uint8Array(0) };

      const pos = Number(offset);
      const available = node.data!.length - pos;
      if (available <= 0) return { errno: ERRNO_SUCCESS, data: new Uint8Array(0) };

      const n = Math.min(maxBytes, available);
      const result = node.data!.subarray(pos, pos + n);
      return { errno: ERRNO_SUCCESS, data: result };
    },

    fdPwrite(fd, writeData, offset) {
      const entry = fdTable.get(fd);
      if (!entry) return { errno: ERRNO_EBADF, written: 0 };
      if (entry.resource.type !== 'vfsFile') return { errno: ERRNO_EBADF, written: 0 };

      const node = vfs.getInodeByIno(entry.resource.ino);
      if (!node || node.type !== 'file') return { errno: ERRNO_EBADF, written: 0 };

      const pos = Number(offset);
      const endPos = pos + writeData.length;
      if (endPos > node.data!.length) {
        const newData = new Uint8Array(endPos);
        newData.set(node.data!);
        node.data = newData;
      }
      node.data!.set(writeData, pos);
      node.mtime = Date.now();
      return { errno: ERRNO_SUCCESS, written: writeData.length };
    },
  };
}

// ---------------------------------------------------------------------------
// Standalone process I/O bridge
// ---------------------------------------------------------------------------

/**
 * Create a standalone process I/O bridge that wraps WasiFDTable + options.
 * Moves args/env/fdstat/proc_exit logic out of the polyfill.
 */
export function createStandaloneProcessIO(
  fdTable: WasiFDTable,
  args: string[],
  env: Record<string, string>,
): WasiProcessIO {
  let exitCode: number | null = null;

  return {
    getArgs() {
      return args;
    },

    getEnviron() {
      return env;
    },

    fdFdstatGet(fd) {
      const entry = fdTable.get(fd);
      if (!entry) {
        return { errno: ERRNO_EBADF, filetype: 0, fdflags: 0, rightsBase: 0n, rightsInheriting: 0n };
      }
      return {
        errno: ERRNO_SUCCESS,
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
      entry.fdflags = flags;
      return ERRNO_SUCCESS;
    },

    procExit(code) {
      exitCode = code;
    },
  };
}
