import { describe, it, beforeEach, expect } from 'vitest';
import { FDTable, FILETYPE_REGULAR_FILE, FILETYPE_DIRECTORY, FILETYPE_CHARACTER_DEVICE,
  FILETYPE_SYMBOLIC_LINK, FDFLAG_APPEND, ERRNO_SUCCESS, ERRNO_EBADF, ERRNO_EINVAL,
  RIGHT_FD_READ, RIGHT_FD_WRITE, RIGHT_FD_SEEK, RIGHT_FD_READDIR,
  RIGHT_FD_FILESTAT_GET, RIGHT_FD_FILESTAT_SET_SIZE, RIGHT_FD_FILESTAT_SET_TIMES,
  RIGHT_PATH_OPEN, RIGHT_PATH_CREATE_DIRECTORY, RIGHT_PATH_UNLINK_FILE,
  RIGHT_PATH_REMOVE_DIRECTORY, RIGHT_PATH_RENAME_SOURCE, RIGHT_PATH_RENAME_TARGET,
  RIGHT_PATH_SYMLINK, RIGHT_PATH_READLINK, RIGHT_PATH_FILESTAT_GET,
  RIGHT_PATH_FILESTAT_SET_TIMES, RIGHT_PATH_CREATE_FILE } from './helpers/test-fd-table.ts';
import { VFS } from './helpers/test-vfs.ts';
import { createStandaloneFileIO, createStandaloneProcessIO } from './helpers/test-bridges.ts';
import { WasiPolyfill, ERRNO_ESPIPE, ERRNO_EISDIR, ERRNO_ENOENT, ERRNO_EEXIST,
  ERRNO_ENOTDIR, ERRNO_ENOTEMPTY } from '../src/wasi-polyfill.ts';

// --- Test helpers ---

interface MockMemory {
  buffer: ArrayBuffer;
}

function createMockMemory(size = 65536): MockMemory {
  return { buffer: new ArrayBuffer(size) };
}

/** Write a string into memory at ptr, return the bytes written. */
function writeString(memory: MockMemory, ptr: number, str: string): number {
  const encoded = new TextEncoder().encode(str);
  new Uint8Array(memory.buffer).set(encoded, ptr);
  return encoded.length;
}

/** Read a string from memory at ptr with given length. */
function readString(memory: MockMemory, ptr: number, len: number): string {
  const bytes = new Uint8Array(memory.buffer, ptr, len);
  return new TextDecoder().decode(bytes);
}

/** Read u32 from memory (LE). */
function readU32(memory: MockMemory, ptr: number): number {
  return new DataView(memory.buffer).getUint32(ptr, true);
}

/** Read u64 from memory (LE) as BigInt. */
function readU64(memory: MockMemory, ptr: number): bigint {
  return new DataView(memory.buffer).getBigUint64(ptr, true);
}

/** Read u8 from memory. */
function readU8(memory: MockMemory, ptr: number): number {
  return new DataView(memory.buffer).getUint8(ptr);
}

/** Create a standard test setup. */
function createTestSetup(options: Record<string, unknown> = {}) {
  const fdTable = new FDTable();
  const vfs = new VFS();
  const memory = createMockMemory();
  const args = (options.args as string[] | undefined) ?? [];
  const env = (options.env as Record<string, string> | undefined) ?? {};
  const fileIO = createStandaloneFileIO(fdTable, vfs);
  const processIO = createStandaloneProcessIO(fdTable, args, env);
  const wasi = new WasiPolyfill(fdTable, vfs, { fileIO, processIO, memory, ...options });
  return { fdTable, vfs, memory, wasi };
}

/** Write a path string into memory and call a path_* function using dirfd 3 (root preopen). */
function writePath(memory: MockMemory, ptr: number, path: string): number {
  return writeString(memory, ptr, path);
}

// --- Tests ---

describe('WasiPolyfill - Path Operations (US-008)', () => {

  describe('path_open', () => {
    it('opens an existing file', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/hello.txt', 'hello world');

      const pathLen = writePath(memory, 1000, 'tmp/hello.txt');
      const errno = wasi.path_open(3, 1, 1000, pathLen, 0, RIGHT_FD_READ | RIGHT_FD_WRITE, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_SUCCESS);
      const fd = readU32(memory, 2000);
      expect(fd >= 4).toBeTruthy();
    });

    it('creates a file with OFLAG_CREAT', () => {
      const { wasi, memory, vfs } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/newfile.txt');
      // oflags = 1 (OFLAG_CREAT)
      const errno = wasi.path_open(3, 1, 1000, pathLen, 1, RIGHT_FD_READ | RIGHT_FD_WRITE, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(vfs.exists('/tmp/newfile.txt')).toBeTruthy();
    });

    it('returns ENOENT for non-existent file without CREAT', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/nope.txt');
      const errno = wasi.path_open(3, 1, 1000, pathLen, 0, RIGHT_FD_READ, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_ENOENT);
    });

    it('returns EEXIST with CREAT|EXCL on existing file', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/exists.txt', 'data');

      const pathLen = writePath(memory, 1000, 'tmp/exists.txt');
      // oflags = 1|4 = 5 (OFLAG_CREAT | OFLAG_EXCL)
      const errno = wasi.path_open(3, 1, 1000, pathLen, 5, RIGHT_FD_READ, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_EEXIST);
    });

    it('truncates file with OFLAG_TRUNC', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/trunc.txt', 'some data here');

      const pathLen = writePath(memory, 1000, 'tmp/trunc.txt');
      // oflags = 8 (OFLAG_TRUNC)
      const errno = wasi.path_open(3, 1, 1000, pathLen, 8, RIGHT_FD_READ | RIGHT_FD_WRITE, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_SUCCESS);

      const content = vfs.readFile('/tmp/trunc.txt');
      expect(content.length).toBe(0);
    });

    it('opens a directory with OFLAG_DIRECTORY', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/mydir');

      const pathLen = writePath(memory, 1000, 'tmp/mydir');
      // oflags = 2 (OFLAG_DIRECTORY)
      const errno = wasi.path_open(3, 1, 1000, pathLen, 2, RIGHT_FD_READDIR, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_SUCCESS);
    });

    it('returns ENOTDIR with OFLAG_DIRECTORY on a file', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/notdir.txt', 'data');

      const pathLen = writePath(memory, 1000, 'tmp/notdir.txt');
      // oflags = 2 (OFLAG_DIRECTORY)
      const errno = wasi.path_open(3, 1, 1000, pathLen, 2, RIGHT_FD_READ, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_ENOTDIR);
    });

    it('returns EBADF for invalid dirfd', () => {
      const { wasi, memory } = createTestSetup();
      const pathLen = writePath(memory, 1000, 'tmp/x.txt');
      const errno = wasi.path_open(99, 1, 1000, pathLen, 0, RIGHT_FD_READ, 0n, 0, 2000);
      expect(errno).toBe(ERRNO_EBADF);
    });
  });

  describe('path_create_directory', () => {
    it('creates a directory', () => {
      const { wasi, memory, vfs } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/newdir');
      const errno = wasi.path_create_directory(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(vfs.exists('/tmp/newdir')).toBeTruthy();
      const stat = vfs.stat('/tmp/newdir');
      expect(stat.type).toBe('dir');
    });

    it('returns EEXIST if directory already exists', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/existing');

      const pathLen = writePath(memory, 1000, 'tmp/existing');
      const errno = wasi.path_create_directory(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_EEXIST);
    });

    it('returns ENOENT if parent does not exist', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'nonexistent/subdir');
      const errno = wasi.path_create_directory(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_ENOENT);
    });

    it('returns EBADF for invalid dirfd', () => {
      const { wasi, memory } = createTestSetup();
      const pathLen = writePath(memory, 1000, 'tmp/dir');
      const errno = wasi.path_create_directory(99, 1000, pathLen);
      expect(errno).toBe(ERRNO_EBADF);
    });
  });

  describe('path_unlink_file', () => {
    it('unlinks a file', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/deleteme.txt', 'goodbye');

      const pathLen = writePath(memory, 1000, 'tmp/deleteme.txt');
      const errno = wasi.path_unlink_file(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(vfs.exists('/tmp/deleteme.txt')).toBeFalsy();
    });

    it('returns ENOENT for non-existent file', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/nope.txt');
      const errno = wasi.path_unlink_file(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_ENOENT);
    });

    it('returns EISDIR when unlinking a directory', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/adir');

      const pathLen = writePath(memory, 1000, 'tmp/adir');
      const errno = wasi.path_unlink_file(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_EISDIR);
    });
  });

  describe('path_remove_directory', () => {
    it('removes an empty directory', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/emptydir');

      const pathLen = writePath(memory, 1000, 'tmp/emptydir');
      const errno = wasi.path_remove_directory(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(vfs.exists('/tmp/emptydir')).toBeFalsy();
    });

    it('returns ENOTEMPTY for non-empty directory', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/fulldir');
      vfs.writeFile('/tmp/fulldir/file.txt', 'data');

      const pathLen = writePath(memory, 1000, 'tmp/fulldir');
      const errno = wasi.path_remove_directory(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_ENOTEMPTY);
    });

    it('returns ENOTDIR for a file', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/notadir.txt', 'data');

      const pathLen = writePath(memory, 1000, 'tmp/notadir.txt');
      const errno = wasi.path_remove_directory(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_ENOTDIR);
    });

    it('returns ENOENT for non-existent path', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/nope');
      const errno = wasi.path_remove_directory(3, 1000, pathLen);
      expect(errno).toBe(ERRNO_ENOENT);
    });
  });

  describe('path_rename', () => {
    it('renames a file', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/old.txt', 'content');

      const oldLen = writePath(memory, 1000, 'tmp/old.txt');
      const newLen = writePath(memory, 2000, 'tmp/new.txt');
      const errno = wasi.path_rename(3, 1000, oldLen, 3, 2000, newLen);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(vfs.exists('/tmp/old.txt')).toBeFalsy();
      expect(vfs.exists('/tmp/new.txt')).toBeTruthy();
      const content = new TextDecoder().decode(vfs.readFile('/tmp/new.txt'));
      expect(content).toBe('content');
    });

    it('returns ENOENT for non-existent source', () => {
      const { wasi, memory } = createTestSetup();

      const oldLen = writePath(memory, 1000, 'tmp/nope.txt');
      const newLen = writePath(memory, 2000, 'tmp/new.txt');
      const errno = wasi.path_rename(3, 1000, oldLen, 3, 2000, newLen);
      expect(errno).toBe(ERRNO_ENOENT);
    });
  });

  describe('path_symlink', () => {
    it('creates a symbolic link', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/target.txt', 'target content');

      const targetLen = writePath(memory, 1000, '/tmp/target.txt');
      const linkLen = writePath(memory, 2000, 'tmp/link.txt');
      const errno = wasi.path_symlink(1000, targetLen, 3, 2000, linkLen);
      expect(errno).toBe(ERRNO_SUCCESS);

      const linkTarget = vfs.readlink('/tmp/link.txt');
      expect(linkTarget).toBe('/tmp/target.txt');
    });

    it('returns EEXIST if link path already exists', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/existing.txt', 'data');

      const targetLen = writePath(memory, 1000, '/tmp/somewhere');
      const linkLen = writePath(memory, 2000, 'tmp/existing.txt');
      const errno = wasi.path_symlink(1000, targetLen, 3, 2000, linkLen);
      expect(errno).toBe(ERRNO_EEXIST);
    });
  });

  describe('path_readlink', () => {
    it('reads a symbolic link target', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.symlink('/tmp/target', '/tmp/mylink');

      const pathLen = writePath(memory, 1000, 'tmp/mylink');
      const errno = wasi.path_readlink(3, 1000, pathLen, 3000, 256, 4000);
      expect(errno).toBe(ERRNO_SUCCESS);

      const used = readU32(memory, 4000);
      expect(used).toBe('/tmp/target'.length);
      expect(readString(memory, 3000, used)).toBe('/tmp/target');
    });

    it('returns EINVAL for non-symlink', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/regular.txt', 'data');

      const pathLen = writePath(memory, 1000, 'tmp/regular.txt');
      const errno = wasi.path_readlink(3, 1000, pathLen, 3000, 256, 4000);
      expect(errno).toBe(ERRNO_EINVAL);
    });

    it('returns ENOENT for non-existent path', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/nope');
      const errno = wasi.path_readlink(3, 1000, pathLen, 3000, 256, 4000);
      expect(errno).toBe(ERRNO_ENOENT);
    });

    it('truncates if buffer is too small', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.symlink('/a/very/long/target/path', '/tmp/longlink');

      const pathLen = writePath(memory, 1000, 'tmp/longlink');
      const errno = wasi.path_readlink(3, 1000, pathLen, 3000, 5, 4000);
      expect(errno).toBe(ERRNO_SUCCESS);

      const used = readU32(memory, 4000);
      expect(used).toBe(5);
      expect(readString(memory, 3000, 5)).toBe('/a/ve');
    });
  });

  describe('path_filestat_get', () => {
    it('returns filestat for a regular file', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/stat.txt', 'hello world');

      const pathLen = writePath(memory, 1000, 'tmp/stat.txt');
      // flags = 1 (LOOKUP_SYMLINK_FOLLOW)
      const errno = wasi.path_filestat_get(3, 1, 1000, pathLen, 5000);
      expect(errno).toBe(ERRNO_SUCCESS);

      // ino (offset 8) should be non-zero
      const ino = readU64(memory, 5008);
      expect(ino > 0n).toBeTruthy();
      // filetype (offset 16) = REGULAR_FILE = 4
      expect(readU8(memory, 5016)).toBe(FILETYPE_REGULAR_FILE);
      // size (offset 32) = 11
      expect(readU64(memory, 5032)).toBe(11n);
      // nlink (offset 24) = 1
      expect(readU64(memory, 5024)).toBe(1n);
      // timestamps should be non-zero (in nanoseconds)
      expect(readU64(memory, 5040) > 0n).toBeTruthy(); // atim
      expect(readU64(memory, 5048) > 0n).toBeTruthy(); // mtim
      expect(readU64(memory, 5056) > 0n).toBeTruthy(); // ctim
    });

    it('returns filestat for a directory', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp');
      const errno = wasi.path_filestat_get(3, 1, 1000, pathLen, 5000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU8(memory, 5016)).toBe(FILETYPE_DIRECTORY);
    });

    it('returns ENOENT for non-existent path', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/nope');
      const errno = wasi.path_filestat_get(3, 1, 1000, pathLen, 5000);
      expect(errno).toBe(ERRNO_ENOENT);
    });

    it('follows symlinks when flag is set', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/real.txt', 'real content');
      vfs.symlink('/tmp/real.txt', '/tmp/symlink.txt');

      const pathLen = writePath(memory, 1000, 'tmp/symlink.txt');
      // flags = 1 (LOOKUP_SYMLINK_FOLLOW)
      const errno = wasi.path_filestat_get(3, 1, 1000, pathLen, 5000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU8(memory, 5016)).toBe(FILETYPE_REGULAR_FILE);
      expect(readU64(memory, 5032)).toBe(12n); // size of 'real content'
    });

    it('returns symlink stat when follow flag not set', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/real.txt', 'real content');
      vfs.symlink('/tmp/real.txt', '/tmp/symlink2.txt');

      const pathLen = writePath(memory, 1000, 'tmp/symlink2.txt');
      // flags = 0 (no symlink follow)
      const errno = wasi.path_filestat_get(3, 0, 1000, pathLen, 5000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU8(memory, 5016)).toBe(FILETYPE_SYMBOLIC_LINK);
    });
  });

  describe('path_filestat_set_times', () => {
    it('sets atim and mtim to specific values', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/times.txt', 'data');

      const pathLen = writePath(memory, 1000, 'tmp/times.txt');
      // fst_flags = 1|4 = 5 (FSTFLAG_ATIM | FSTFLAG_MTIM)
      // atim = 1000000000n ns = 1000ms, mtim = 2000000000n ns = 2000ms
      const errno = wasi.path_filestat_set_times(3, 1, 1000, pathLen,
        1000000000n, 2000000000n, 5);
      expect(errno).toBe(ERRNO_SUCCESS);

      const stat = vfs.stat('/tmp/times.txt');
      expect(stat.atime).toBe(1000);
      expect(stat.mtime).toBe(2000);
    });

    it('sets times to now with FSTFLAG_*_NOW', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.writeFile('/tmp/now.txt', 'data');

      // Manually set old timestamps
      const ino = vfs.getIno('/tmp/now.txt');
      const node = vfs.getInodeByIno(ino!);
      (node as { atime: number }).atime = 100;
      (node as { mtime: number }).mtime = 200;

      const pathLen = writePath(memory, 1000, 'tmp/now.txt');
      // fst_flags = 2|8 = 10 (FSTFLAG_ATIM_NOW | FSTFLAG_MTIM_NOW)
      const before = Date.now();
      const errno = wasi.path_filestat_set_times(3, 1, 1000, pathLen, 0n, 0n, 10);
      expect(errno).toBe(ERRNO_SUCCESS);

      const stat = vfs.stat('/tmp/now.txt');
      expect(stat.atime >= before).toBeTruthy();
      expect(stat.mtime >= before).toBeTruthy();
    });

    it('returns ENOENT for non-existent path', () => {
      const { wasi, memory } = createTestSetup();

      const pathLen = writePath(memory, 1000, 'tmp/nope.txt');
      const errno = wasi.path_filestat_set_times(3, 1, 1000, pathLen, 0n, 0n, 10);
      expect(errno).toBe(ERRNO_ENOENT);
    });
  });

  describe('fd_filestat_get', () => {
    it('returns filestat for VFS file fd', () => {
      const { wasi, memory, vfs, fdTable } = createTestSetup();
      vfs.writeFile('/tmp/stat.txt', 'hello');
      const ino = vfs.getIno('/tmp/stat.txt')!;
      const fd = fdTable.open(
        { type: 'vfsFile', ino, path: '/tmp/stat.txt' },
        { filetype: FILETYPE_REGULAR_FILE }
      );

      const errno = wasi.fd_filestat_get(fd, 5000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU8(memory, 5016)).toBe(FILETYPE_REGULAR_FILE);
      expect(readU64(memory, 5032)).toBe(5n); // size
      expect(readU64(memory, 5008)).toBe(BigInt(ino)); // ino
    });

    it('returns filestat for preopen directory fd', () => {
      const { wasi, memory } = createTestSetup();

      const errno = wasi.fd_filestat_get(3, 5000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU8(memory, 5016)).toBe(FILETYPE_DIRECTORY);
    });

    it('returns minimal stat for stdio fd', () => {
      const { wasi, memory } = createTestSetup();

      const errno = wasi.fd_filestat_get(1, 5000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU8(memory, 5016)).toBe(FILETYPE_CHARACTER_DEVICE);
      expect(readU64(memory, 5032)).toBe(0n); // size = 0
    });

    it('returns EBADF for invalid fd', () => {
      const { wasi } = createTestSetup();
      const errno = wasi.fd_filestat_get(99, 5000);
      expect(errno).toBe(ERRNO_EBADF);
    });
  });

  describe('fd_filestat_set_size', () => {
    it('truncates a file', () => {
      const { wasi, vfs, fdTable } = createTestSetup();
      vfs.writeFile('/tmp/trunc.txt', 'hello world');
      const ino = vfs.getIno('/tmp/trunc.txt')!;
      const fd = fdTable.open(
        { type: 'vfsFile', ino, path: '/tmp/trunc.txt' },
        { filetype: FILETYPE_REGULAR_FILE }
      );

      const errno = wasi.fd_filestat_set_size(fd, 5n);
      expect(errno).toBe(ERRNO_SUCCESS);
      const content = new TextDecoder().decode(vfs.readFile('/tmp/trunc.txt'));
      expect(content).toBe('hello');
    });

    it('extends a file with zeros', () => {
      const { wasi, vfs, fdTable } = createTestSetup();
      vfs.writeFile('/tmp/extend.txt', 'AB');
      const ino = vfs.getIno('/tmp/extend.txt')!;
      const fd = fdTable.open(
        { type: 'vfsFile', ino, path: '/tmp/extend.txt' },
        { filetype: FILETYPE_REGULAR_FILE }
      );

      const errno = wasi.fd_filestat_set_size(fd, 5n);
      expect(errno).toBe(ERRNO_SUCCESS);
      const data = vfs.readFile('/tmp/extend.txt');
      expect(data.length).toBe(5);
      expect(data[0]).toBe(65); // 'A'
      expect(data[1]).toBe(66); // 'B'
      expect(data[2]).toBe(0);
      expect(data[3]).toBe(0);
      expect(data[4]).toBe(0);
    });

    it('resolves path-backed file descriptors whose inode is synthesized later', () => {
      const { wasi, vfs, fdTable } = createTestSetup();
      vfs.writeFile('/tmp/kernel-opened.txt', 'abcdef');
      const fd = fdTable.open(
        { type: 'vfsFile', ino: 0, path: '/tmp/kernel-opened.txt' },
        { filetype: FILETYPE_REGULAR_FILE }
      );

      const errno = wasi.fd_filestat_set_size(fd, 3n);
      expect(errno).toBe(ERRNO_SUCCESS);
      const content = new TextDecoder().decode(vfs.readFile('/tmp/kernel-opened.txt'));
      expect(content).toBe('abc');
    });

    it('returns EBADF for invalid fd', () => {
      const { wasi } = createTestSetup();
      const errno = wasi.fd_filestat_set_size(99, 0n);
      expect(errno).toBe(ERRNO_EBADF);
    });

    it('returns EINVAL for non-vfsFile fd', () => {
      const { wasi } = createTestSetup();
      // fd 1 is stdout (stdio)
      const errno = wasi.fd_filestat_set_size(1, 0n);
      expect(errno).toBe(ERRNO_EBADF);
    });
  });

  describe('fd_filestat_set_times', () => {
    it('sets timestamps on VFS file fd', () => {
      const { wasi, vfs, fdTable } = createTestSetup();
      vfs.writeFile('/tmp/times.txt', 'data');
      const ino = vfs.getIno('/tmp/times.txt')!;
      const fd = fdTable.open(
        { type: 'vfsFile', ino, path: '/tmp/times.txt' },
        { filetype: FILETYPE_REGULAR_FILE }
      );

      // fst_flags = 1|4 = 5 (FSTFLAG_ATIM | FSTFLAG_MTIM)
      const errno = wasi.fd_filestat_set_times(fd, 3000000000n, 4000000000n, 5);
      expect(errno).toBe(ERRNO_SUCCESS);

      const stat = vfs.stat('/tmp/times.txt');
      expect(stat.atime).toBe(3000);
      expect(stat.mtime).toBe(4000);
    });

    it('returns EBADF for stdio fd (no set_times rights)', () => {
      const { wasi } = createTestSetup();
      const errno = wasi.fd_filestat_set_times(1, 0n, 0n, 10);
      expect(errno).toBe(ERRNO_EBADF);
    });

    it('returns EBADF for invalid fd', () => {
      const { wasi } = createTestSetup();
      const errno = wasi.fd_filestat_set_times(99, 0n, 0n, 10);
      expect(errno).toBe(ERRNO_EBADF);
    });
  });

  describe('fd_readdir', () => {
    it('reads directory entries from root preopen', () => {
      const { wasi, memory } = createTestSetup();

      // Root directory contains: bin, tmp, home, dev
      const errno = wasi.fd_readdir(3, 5000, 4096, 0n, 6000);
      expect(errno).toBe(ERRNO_SUCCESS);

      const bufused = readU32(memory, 6000);
      expect(bufused > 0).toBeTruthy();
    });

    it('reads entries with correct dirent struct format', () => {
      const { wasi, memory, vfs } = createTestSetup();
      // Create a simple directory with known entries
      vfs.mkdir('/tmp/readdir_test');
      vfs.writeFile('/tmp/readdir_test/a.txt', 'aaa');
      vfs.writeFile('/tmp/readdir_test/b.txt', 'bbb');

      // Open the directory via path_open
      const pathLen = writePath(memory, 100, 'tmp/readdir_test');
      wasi.path_open(3, 1, 100, pathLen, 2, RIGHT_FD_READDIR, 0n, 0, 200);
      const dirfd = readU32(memory, 200);

      const errno = wasi.fd_readdir(dirfd, 5000, 4096, 0n, 6000);
      expect(errno).toBe(ERRNO_SUCCESS);

      const bufused = readU32(memory, 6000);
      expect(bufused > 0).toBeTruthy();

      // First entry: header (24 bytes) + name
      const d_next = readU64(memory, 5000);
      expect(d_next).toBe(1n);
      const d_namlen = readU32(memory, 5016);
      expect(d_namlen).toBe(5); // 'a.txt'
      const d_type = readU8(memory, 5020);
      expect(d_type).toBe(FILETYPE_REGULAR_FILE);
      const name = readString(memory, 5024, 5);
      expect(name).toBe('a.txt');

      // Second entry starts at 5024 + 5 = 5029
      const d_next2 = readU64(memory, 5029);
      expect(d_next2).toBe(2n);
      const d_namlen2 = readU32(memory, 5029 + 16);
      expect(d_namlen2).toBe(5); // 'b.txt'
      const name2 = readString(memory, 5029 + 24, 5);
      expect(name2).toBe('b.txt');
    });

    it('supports cookie-based pagination', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/pagedir');
      vfs.writeFile('/tmp/pagedir/first.txt', '1');
      vfs.writeFile('/tmp/pagedir/second.txt', '2');

      const pathLen = writePath(memory, 100, 'tmp/pagedir');
      wasi.path_open(3, 1, 100, pathLen, 2, RIGHT_FD_READDIR, 0n, 0, 200);
      const dirfd = readU32(memory, 200);

      // Read starting at cookie 1 (skip first entry)
      const errno = wasi.fd_readdir(dirfd, 5000, 4096, 1n, 6000);
      expect(errno).toBe(ERRNO_SUCCESS);

      const bufused = readU32(memory, 6000);
      expect(bufused > 0).toBeTruthy();

      // Should get 'second.txt' as first entry
      const d_namlen = readU32(memory, 5016);
      expect(d_namlen).toBe(10); // 'second.txt'
      const name = readString(memory, 5024, 10);
      expect(name).toBe('second.txt');
    });

    it('handles small buffer', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/smallbuf');
      vfs.writeFile('/tmp/smallbuf/file.txt', 'x');

      const pathLen = writePath(memory, 100, 'tmp/smallbuf');
      wasi.path_open(3, 1, 100, pathLen, 2, RIGHT_FD_READDIR, 0n, 0, 200);
      const dirfd = readU32(memory, 200);

      // Buffer too small for even one header (24 bytes)
      const errno = wasi.fd_readdir(dirfd, 5000, 10, 0n, 6000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU32(memory, 6000)).toBe(0); // nothing written
    });

    it('returns EBADF for invalid fd', () => {
      const { wasi, memory } = createTestSetup();
      const errno = wasi.fd_readdir(99, 5000, 4096, 0n, 6000);
      expect(errno).toBe(ERRNO_EBADF);
    });

    it('returns ENOTDIR for non-directory fd', () => {
      const { wasi, memory, vfs, fdTable } = createTestSetup();
      vfs.writeFile('/tmp/notdir.txt', 'data');
      const ino = vfs.getIno('/tmp/notdir.txt')!;
      const fd = fdTable.open(
        { type: 'vfsFile', ino, path: '/tmp/notdir.txt' },
        { filetype: FILETYPE_REGULAR_FILE, rightsBase: RIGHT_FD_READDIR }
      );

      const errno = wasi.fd_readdir(fd, 5000, 4096, 0n, 6000);
      expect(errno).toBe(ERRNO_ENOTDIR);
    });

    it('returns 0 bufused for empty directory', () => {
      const { wasi, memory, vfs } = createTestSetup();
      vfs.mkdir('/tmp/emptydir');

      const pathLen = writePath(memory, 100, 'tmp/emptydir');
      wasi.path_open(3, 1, 100, pathLen, 2, RIGHT_FD_READDIR, 0n, 0, 200);
      const dirfd = readU32(memory, 200);

      const errno = wasi.fd_readdir(dirfd, 5000, 4096, 0n, 6000);
      expect(errno).toBe(ERRNO_SUCCESS);
      expect(readU32(memory, 6000)).toBe(0);
    });
  });

  describe('getImports - path operations', () => {
    it('returns all US-008 WASI functions', () => {
      const { wasi } = createTestSetup();
      const imports = wasi.getImports() as Record<string, unknown>;
      const expectedFns = [
        // US-007
        'fd_read', 'fd_write', 'fd_seek', 'fd_tell', 'fd_close',
        'fd_fdstat_get', 'fd_fdstat_set_flags',
        'fd_prestat_get', 'fd_prestat_dir_name',
        // US-008
        'path_open', 'path_create_directory', 'path_unlink_file',
        'path_remove_directory', 'path_rename', 'path_symlink',
        'path_readlink', 'path_filestat_get', 'path_filestat_set_times',
        'fd_filestat_get', 'fd_filestat_set_size', 'fd_filestat_set_times',
        'fd_readdir',
      ];
      for (const name of expectedFns) {
        expect(typeof imports[name]).toBe('function');
      }
    });
  });
});
