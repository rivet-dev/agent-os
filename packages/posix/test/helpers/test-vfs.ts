/**
 * In-memory virtual filesystem with inode-based storage (test helper).
 *
 * Concrete implementation of the WasiVFS interface for unit testing.
 * Supports directories, regular files, symlinks, and special device nodes.
 * Pre-populated with standard Unix layout.
 */

import type { VfsStat, VfsSnapshotEntry, WasiVFS, WasiInode } from '../../src/wasi-types.ts';
export { VfsError } from '../../src/wasi-types.ts';
export type { VfsErrorCode } from '../../src/wasi-types.ts';
import { VfsError } from '../../src/wasi-types.ts';

// Inode types
const INODE_FILE = 'file' as const;
const INODE_DIR = 'dir' as const;
const INODE_SYMLINK = 'symlink' as const;
const INODE_DEV = 'dev' as const;

type InodeType = typeof INODE_FILE | typeof INODE_DIR | typeof INODE_SYMLINK | typeof INODE_DEV;

type DevType = 'null' | 'stdin' | 'stdout' | 'stderr';

// Default permissions
const DEFAULT_FILE_MODE: number = 0o644;
const DEFAULT_DIR_MODE: number = 0o755;
const DEFAULT_SYMLINK_MODE: number = 0o777;

// Max symlink follow depth to prevent loops
const MAX_SYMLINK_DEPTH: number = 40;

/**
 * An inode representing a filesystem object.
 */
class Inode implements WasiInode {
  type: InodeType;
  mode: number;
  uid: number;
  gid: number;
  nlink: number;
  atime: number;
  mtime: number;
  ctime: number;

  // Type-specific data (optional depending on inode type)
  data?: Uint8Array;
  entries?: Map<string, number>;
  target?: string;
  devType?: DevType;

  constructor(type: InodeType, mode: number) {
    this.type = type;
    this.mode = mode;
    this.uid = 1000;
    this.gid = 1000;
    this.nlink = type === INODE_DIR ? 2 : 1; // dirs start with 2 (self + parent)
    const now = Date.now();
    this.atime = now;
    this.mtime = now;
    this.ctime = now;

    // Type-specific data
    if (type === INODE_FILE) {
      this.data = new Uint8Array(0);
    } else if (type === INODE_DIR) {
      this.entries = new Map();
    } else if (type === INODE_SYMLINK) {
      this.target = '';
    } else if (type === INODE_DEV) {
      this.devType = undefined;
    }
  }

  get size(): number {
    if (this.type === INODE_FILE) return this.data!.length;
    if (this.type === INODE_SYMLINK) return new TextEncoder().encode(this.target!).length;
    if (this.type === INODE_DIR) return this.entries!.size;
    return 0;
  }
}

/**
 * In-memory virtual filesystem.
 */
export class VFS implements WasiVFS {
  private _inodes: Map<number, Inode>;
  private _nextIno: number;
  private _root: number;

  constructor() {
    this._inodes = new Map();
    this._nextIno = 1;

    // Create root directory
    const rootIno = this._allocInode(INODE_DIR, DEFAULT_DIR_MODE);
    this._root = rootIno;

    // Pre-populate standard directories and devices
    this._initLayout();
  }

  private _allocInode(type: InodeType, mode: number): number {
    const ino = this._nextIno++;
    this._inodes.set(ino, new Inode(type, mode));
    return ino;
  }

  private _getInode(ino: number): Inode | null {
    return this._inodes.get(ino) ?? null;
  }

  private _initLayout(): void {
    // Standard directories
    this.mkdirp('/bin');
    this.mkdirp('/tmp');
    this.mkdirp('/home/user');
    this.mkdirp('/dev');

    // Device nodes
    this._createDev('/dev/null', 'null');
    this._createDev('/dev/stdin', 'stdin');
    this._createDev('/dev/stdout', 'stdout');
    this._createDev('/dev/stderr', 'stderr');

    // Populate /bin with executable stubs for all known commands.
    // brush-shell searches PATH for external commands; without
    // these stubs it returns 127 (command not found). The actual execution is
    // handled by proc_spawn creating a new WASM instance that dispatches
    // based on argv[0].
    this._populateBin();
  }

  /**
   * Create empty executable stubs in /bin for all supported commands.
   * brush-shell's PATH lookup needs these to resolve external commands.
   */
  private _populateBin(): void {
    const commands = [
      // Shell
      'sh', 'bash',
      // Text processing
      'grep', 'egrep', 'fgrep', 'rg', 'sed', 'awk', 'jq', 'yq',
      // Find
      'find',
      // Built-in implementations
      'cat', 'chmod', 'column', 'cp', 'dd', 'diff', 'du', 'expr', 'file', 'head',
      'ln', 'logname', 'ls', 'mkdir', 'mktemp', 'mv', 'pathchk', 'rev', 'rm',
      'sleep', 'sort', 'split', 'stat', 'strings', 'tac', 'tail', 'test',
      '[', 'touch', 'tree', 'tsort', 'whoami',
      // Compression & Archiving
      'gzip', 'gunzip', 'zcat', 'tar',
      // Shim commands
      'env', 'nice', 'nohup', 'stdbuf', 'timeout', 'xargs',
      // uutils: text/encoding
      'base32', 'base64', 'basenc', 'basename', 'comm', 'cut',
      'dircolors', 'dirname', 'echo', 'expand', 'factor', 'false',
      'fmt', 'fold', 'join', 'nl', 'numfmt', 'od', 'paste',
      'printenv', 'printf', 'ptx', 'seq', 'shuf', 'tr', 'true',
      'unexpand', 'uniq', 'wc', 'yes',
      // uutils: checksums
      'b2sum', 'cksum', 'md5sum', 'sha1sum', 'sha224sum', 'sha256sum',
      'sha384sum', 'sha512sum', 'sum',
      // uutils: file operations
      'link', 'pwd', 'readlink', 'realpath', 'rmdir', 'shred', 'tee',
      'truncate', 'unlink',
      // uutils: system info
      'arch', 'date', 'nproc', 'uname',
      // uutils: ls variants
      'dir', 'vdir',
      // Stubbed commands (partial or no-op implementations)
      'hostname', 'hostid', 'more', 'sync', 'tty',
      'chcon', 'runcon',
      'chgrp', 'chown',
      'chroot',
      'df',
      'groups', 'id',
      'install',
      'kill',
      'mkfifo', 'mknod',
      'pinky', 'who', 'users', 'uptime',
      'stty',
    ];

    const binDirIno = this._resolve('/bin');
    if (binDirIno === null) return;
    const binDir = this._getInode(binDirIno);
    if (!binDir || binDir.type !== INODE_DIR) return;

    const EXEC_MODE = 0o755;
    for (const cmd of commands) {
      const ino = this._allocInode(INODE_FILE, EXEC_MODE);
      // Leave file data as empty Uint8Array (default from _allocInode)
      binDir.entries!.set(cmd, ino);
    }
  }

  private _createDev(path: string, devType: DevType): void {
    const { dirIno, name } = this._resolveParent(path);
    if (dirIno === null) return;
    const dir = this._getInode(dirIno);
    if (!dir || dir.type !== INODE_DIR) return;

    const ino = this._allocInode(INODE_DEV, DEFAULT_FILE_MODE);
    this._getInode(ino)!.devType = devType;
    dir.entries!.set(name, ino);
  }

  // --- Path resolution ---

  /**
   * Normalize a path: resolve . and .., collapse multiple slashes.
   */
  private _normalizePath(path: string): string {
    if (!path || path === '') return '/';

    // Make absolute
    if (!path.startsWith('/')) {
      path = '/' + path;
    }

    const parts = path.split('/');
    const resolved: string[] = [];

    for (const part of parts) {
      if (part === '' || part === '.') continue;
      if (part === '..') {
        resolved.pop();
      } else {
        resolved.push(part);
      }
    }

    return '/' + resolved.join('/');
  }

  /**
   * Resolve a path to an inode number, optionally following symlinks.
   */
  private _resolve(path: string, followSymlinks: boolean = true, depth: number = 0): number | null {
    if (depth > MAX_SYMLINK_DEPTH) return null;

    path = this._normalizePath(path);
    if (path === '/') return this._root;

    const parts = path.split('/').filter(p => p !== '');
    let currentIno = this._root;

    for (let i = 0; i < parts.length; i++) {
      const node = this._getInode(currentIno);
      if (!node || node.type !== INODE_DIR) return null;

      const childIno = node.entries!.get(parts[i]);
      if (childIno === undefined) return null;

      const child = this._getInode(childIno);
      if (!child) return null;

      if (child.type === INODE_SYMLINK && (followSymlinks || i < parts.length - 1)) {
        // Resolve symlink target
        const targetPath = child.target!.startsWith('/')
          ? child.target!
          : '/' + parts.slice(0, i).join('/') + '/' + child.target!;
        const resolvedIno = this._resolve(targetPath, true, depth + 1);
        if (resolvedIno === null) return null;

        if (i < parts.length - 1) {
          // Continue resolving remaining path components from the symlink target
          const remaining = parts.slice(i + 1).join('/');
          const resolvedNode = this._getInode(resolvedIno);
          if (!resolvedNode || resolvedNode.type !== INODE_DIR) return null;
          const fullPath = this._inoToPath(resolvedIno) + '/' + remaining;
          return this._resolve(fullPath, followSymlinks, depth + 1);
        }
        return resolvedIno;
      }

      currentIno = childIno;
    }

    return currentIno;
  }

  /**
   * Find the inode path for a given inode number (for symlink resolution).
   */
  private _inoToPath(ino: number): string {
    if (ino === this._root) return '/';

    // BFS to find path
    const queue: Array<{ ino: number; path: string }> = [{ ino: this._root, path: '' }];
    while (queue.length > 0) {
      const { ino: curIno, path: curPath } = queue.shift()!;
      const node = this._getInode(curIno);
      if (!node || node.type !== INODE_DIR) continue;

      for (const [name, childIno] of node.entries!) {
        const childPath = curPath + '/' + name;
        if (childIno === ino) return childPath;
        const child = this._getInode(childIno);
        if (child && child.type === INODE_DIR) {
          queue.push({ ino: childIno, path: childPath });
        }
      }
    }
    return '/';
  }

  /**
   * Resolve a path to its parent directory inode and the final name component.
   */
  private _resolveParent(path: string): { dirIno: number | null; name: string } {
    path = this._normalizePath(path);
    if (path === '/') return { dirIno: null, name: '' };

    const lastSlash = path.lastIndexOf('/');
    const parentPath = lastSlash === 0 ? '/' : path.substring(0, lastSlash);
    const name = path.substring(lastSlash + 1);

    const dirIno = this._resolve(parentPath, true);
    return { dirIno, name };
  }

  // --- Public API ---

  /**
   * Check if a path exists.
   */
  exists(path: string): boolean {
    return this._resolve(path, true) !== null;
  }

  /**
   * Create a directory.
   * @throws If parent doesn't exist or path already exists
   */
  mkdir(path: string): void {
    const { dirIno, name } = this._resolveParent(path);
    if (dirIno === null) throw new VfsError('ENOENT', `parent directory does not exist: ${path}`);

    const dir = this._getInode(dirIno);
    if (!dir || dir.type !== INODE_DIR) throw new VfsError('ENOTDIR', `parent is not a directory: ${path}`);
    if (dir.entries!.has(name)) throw new VfsError('EEXIST', `path already exists: ${path}`);

    const ino = this._allocInode(INODE_DIR, DEFAULT_DIR_MODE);
    dir.entries!.set(name, ino);
    dir.nlink++;
    dir.mtime = Date.now();
  }

  /**
   * Create a directory and all necessary parent directories.
   */
  mkdirp(path: string): void {
    path = this._normalizePath(path);
    const parts = path.split('/').filter(p => p !== '');
    let current = '/';
    for (const part of parts) {
      current = current === '/' ? '/' + part : current + '/' + part;
      if (!this.exists(current)) {
        this.mkdir(current);
      }
    }
  }

  /**
   * Write a file. Creates the file if it doesn't exist, overwrites if it does.
   * @throws If parent doesn't exist
   */
  writeFile(path: string, content: Uint8Array | string): void {
    if (typeof content === 'string') {
      content = new TextEncoder().encode(content);
    }

    // Check for device nodes
    const existingIno = this._resolve(path, true);
    if (existingIno !== null) {
      const existing = this._getInode(existingIno)!;
      if (existing.type === INODE_DEV) {
        // /dev/null: discard writes
        if (existing.devType === 'null') return;
        // Other devices: just discard for now
        return;
      }
      if (existing.type === INODE_FILE) {
        existing.data = content instanceof Uint8Array ? content : new Uint8Array(content);
        existing.mtime = Date.now();
        existing.ctime = Date.now();
        return;
      }
      if (existing.type === INODE_DIR) {
        throw new VfsError('EISDIR', `illegal operation on a directory: ${path}`);
      }
    }

    const { dirIno, name } = this._resolveParent(path);
    if (dirIno === null) throw new VfsError('ENOENT', `parent directory does not exist: ${path}`);

    const dir = this._getInode(dirIno);
    if (!dir || dir.type !== INODE_DIR) throw new VfsError('ENOTDIR', `parent is not a directory: ${path}`);

    const ino = this._allocInode(INODE_FILE, DEFAULT_FILE_MODE);
    this._getInode(ino)!.data = content instanceof Uint8Array ? content : new Uint8Array(content);
    dir.entries!.set(name, ino);
    dir.mtime = Date.now();
  }

  /**
   * Resize a file, extending with zero bytes or truncating in place.
   * @throws If the path does not exist or is not a regular file
   */
  truncate(path: string, length: number): void {
    const ino = this._resolve(path, true);
    if (ino === null) throw new VfsError('ENOENT', `no such file: ${path}`);

    const node = this._getInode(ino)!;
    if (node.type !== INODE_FILE) {
      throw new VfsError(node.type === INODE_DIR ? 'EISDIR' : 'EINVAL', `not a regular file: ${path}`);
    }
    if (length < 0) throw new VfsError('EINVAL', `invalid length: ${length}`);
    if (node.data!.length === length) return;

    const next = new Uint8Array(length);
    next.set(node.data!.subarray(0, Math.min(node.data!.length, length)));
    node.data = next;
    node.mtime = Date.now();
    node.ctime = Date.now();
  }

  /**
   * Read a file's contents.
   * @throws If file doesn't exist or is a directory
   */
  readFile(path: string): Uint8Array {
    const ino = this._resolve(path, true);
    if (ino === null) throw new VfsError('ENOENT', `no such file: ${path}`);

    const node = this._getInode(ino)!;
    if (node.type === INODE_DEV) {
      // /dev/null reads as empty
      if (node.devType === 'null') return new Uint8Array(0);
      return new Uint8Array(0);
    }
    if (node.type === INODE_DIR) throw new VfsError('EISDIR', `illegal operation on a directory: ${path}`);
    if (node.type !== INODE_FILE) throw new VfsError('ENOENT', `not a regular file: ${path}`);

    return node.data!;
  }

  /**
   * List directory entries.
   * @throws If path is not a directory
   */
  readdir(path: string): string[] {
    const ino = this._resolve(path, true);
    if (ino === null) throw new VfsError('ENOENT', `no such directory: ${path}`);

    const node = this._getInode(ino)!;
    if (node.type !== INODE_DIR) throw new VfsError('ENOTDIR', `not a directory: ${path}`);

    return Array.from(node.entries!.keys());
  }

  /**
   * Get stat metadata for a path (follows symlinks).
   * @throws If path doesn't exist
   */
  stat(path: string): VfsStat {
    const ino = this._resolve(path, true);
    if (ino === null) throw new VfsError('ENOENT', `no such file or directory: ${path}`);
    return this._statInode(ino);
  }

  /**
   * Get stat metadata for a path (does not follow symlinks).
   * @throws If path doesn't exist
   */
  lstat(path: string): VfsStat {
    const ino = this._resolve(path, false);
    if (ino === null) throw new VfsError('ENOENT', `no such file or directory: ${path}`);
    return this._statInode(ino);
  }

  private _statInode(ino: number): VfsStat {
    const node = this._getInode(ino)!;
    return {
      ino,
      type: node.type,
      mode: node.mode,
      uid: node.uid,
      gid: node.gid,
      nlink: node.nlink,
      size: node.size,
      atime: node.atime,
      mtime: node.mtime,
      ctime: node.ctime,
    };
  }

  /**
   * Remove a file or symlink.
   * @throws If path doesn't exist or is a directory
   */
  unlink(path: string): void {
    const { dirIno, name } = this._resolveParent(path);
    if (dirIno === null) throw new VfsError('ENOENT', `no such file: ${path}`);

    const dir = this._getInode(dirIno);
    if (!dir || dir.type !== INODE_DIR) throw new VfsError('ENOTDIR', `parent is not a directory`);

    const childIno = dir.entries!.get(name);
    if (childIno === undefined) throw new VfsError('ENOENT', `no such file: ${path}`);

    const child = this._getInode(childIno)!;
    if (child.type === INODE_DIR) throw new VfsError('EISDIR', `cannot unlink a directory: ${path}`);

    dir.entries!.delete(name);
    child.nlink--;
    if (child.nlink <= 0) {
      this._inodes.delete(childIno);
    }
    dir.mtime = Date.now();
  }

  /**
   * Remove an empty directory.
   * @throws If path doesn't exist, isn't a directory, or isn't empty
   */
  rmdir(path: string): void {
    path = this._normalizePath(path);
    if (path === '/') throw new VfsError('EPERM', `cannot remove root directory`);

    const { dirIno, name } = this._resolveParent(path);
    if (dirIno === null) throw new VfsError('ENOENT', `no such directory: ${path}`);

    const dir = this._getInode(dirIno)!;
    const childIno = dir.entries!.get(name);
    if (childIno === undefined) throw new VfsError('ENOENT', `no such directory: ${path}`);

    const child = this._getInode(childIno)!;
    if (child.type !== INODE_DIR) throw new VfsError('ENOTDIR', `not a directory: ${path}`);
    if (child.entries!.size > 0) throw new VfsError('ENOTEMPTY', `directory not empty: ${path}`);

    dir.entries!.delete(name);
    dir.nlink--;
    this._inodes.delete(childIno);
    dir.mtime = Date.now();
  }

  /**
   * Rename/move a file or directory.
   * @throws If source doesn't exist or destination parent doesn't exist
   */
  rename(oldPath: string, newPath: string): void {
    const { dirIno: oldDirIno, name: oldName } = this._resolveParent(oldPath);
    if (oldDirIno === null) throw new VfsError('ENOENT', `no such file or directory: ${oldPath}`);

    const oldDir = this._getInode(oldDirIno)!;
    const childIno = oldDir.entries!.get(oldName);
    if (childIno === undefined) throw new VfsError('ENOENT', `no such file or directory: ${oldPath}`);

    const { dirIno: newDirIno, name: newName } = this._resolveParent(newPath);
    if (newDirIno === null) throw new VfsError('ENOENT', `destination parent does not exist: ${newPath}`);

    const newDir = this._getInode(newDirIno);
    if (!newDir || newDir.type !== INODE_DIR) throw new VfsError('ENOTDIR', `destination parent is not a directory`);

    // Remove old entry from destination if it exists
    const existingIno = newDir.entries!.get(newName);
    if (existingIno !== undefined) {
      const existing = this._getInode(existingIno)!;
      if (existing.type === INODE_DIR && existing.entries!.size > 0) {
        throw new VfsError('ENOTEMPTY', `destination directory not empty: ${newPath}`);
      }
      if (existing.type === INODE_DIR) {
        newDir.nlink--;
      }
      this._inodes.delete(existingIno);
    }

    // Move entry
    oldDir.entries!.delete(oldName);
    newDir.entries!.set(newName, childIno);

    const child = this._getInode(childIno)!;
    if (child.type === INODE_DIR) {
      oldDir.nlink--;
      newDir.nlink++;
    }

    oldDir.mtime = Date.now();
    newDir.mtime = Date.now();
    child.ctime = Date.now();
  }

  /**
   * Create a symbolic link.
   * @param target - The symlink target (can be relative or absolute)
   * @param linkPath - Where to create the symlink
   * @throws If parent doesn't exist or linkPath already exists
   */
  symlink(target: string, linkPath: string): void {
    const { dirIno, name } = this._resolveParent(linkPath);
    if (dirIno === null) throw new VfsError('ENOENT', `parent directory does not exist: ${linkPath}`);

    const dir = this._getInode(dirIno);
    if (!dir || dir.type !== INODE_DIR) throw new VfsError('ENOTDIR', `parent is not a directory`);
    if (dir.entries!.has(name)) throw new VfsError('EEXIST', `path already exists: ${linkPath}`);

    const ino = this._allocInode(INODE_SYMLINK, DEFAULT_SYMLINK_MODE);
    this._getInode(ino)!.target = target;
    dir.entries!.set(name, ino);
    dir.mtime = Date.now();
  }

  /**
   * Read the target of a symbolic link.
   * @throws If path doesn't exist or isn't a symlink
   */
  readlink(path: string): string {
    const ino = this._resolve(path, false);
    if (ino === null) throw new VfsError('ENOENT', `no such file: ${path}`);

    const node = this._getInode(ino)!;
    if (node.type !== INODE_SYMLINK) throw new VfsError('EINVAL', `not a symbolic link: ${path}`);

    return node.target!;
  }

  /**
   * Change file permissions.
   * @throws If path doesn't exist
   */
  chmod(path: string, mode: number): void {
    const ino = this._resolve(path, true);
    if (ino === null) throw new VfsError('ENOENT', `no such file or directory: ${path}`);

    const node = this._getInode(ino)!;
    node.mode = mode;
    node.ctime = Date.now();
  }

  /**
   * Get the inode number for a path (used by WASI polyfill).
   */
  getIno(path: string, followSymlinks: boolean = true): number | null {
    return this._resolve(path, followSymlinks);
  }

  /**
   * Get the raw inode for a given inode number (used by WASI polyfill).
   */
  getInodeByIno(ino: number): Inode | null {
    return this._getInode(ino);
  }

  /**
   * Create a snapshot of the entire VFS state.
   * Returns an array of entries suitable for transfer via postMessage.
   * Device nodes are omitted (recreated by constructor).
   */
  snapshot(): VfsSnapshotEntry[] {
    const entries: VfsSnapshotEntry[] = [];
    this._walkForSnapshot(this._root, '/', entries);
    return entries;
  }

  private _walkForSnapshot(ino: number, path: string, entries: VfsSnapshotEntry[]): void {
    const node = this._getInode(ino);
    if (!node) return;

    if (node.type === INODE_DIR) {
      if (path !== '/') {
        entries.push({ type: 'dir', path, mode: node.mode });
      }
      for (const [name, childIno] of node.entries!) {
        const childPath = path === '/' ? '/' + name : path + '/' + name;
        this._walkForSnapshot(childIno, childPath, entries);
      }
    } else if (node.type === INODE_FILE) {
      entries.push({ type: 'file', path, data: new Uint8Array(node.data!), mode: node.mode });
    } else if (node.type === INODE_SYMLINK) {
      entries.push({ type: 'symlink', path, target: node.target! });
    }
    // Skip dev nodes -- recreated by VFS constructor
  }

  /**
   * Create a new VFS from a snapshot.
   */
  static fromSnapshot(entries: VfsSnapshotEntry[]): VFS {
    const vfs = new VFS();
    if (!entries || entries.length === 0) return vfs;
    vfs._applyEntries(entries);
    return vfs;
  }

  /**
   * Replace the contents of this VFS with a snapshot.
   * Resets internal state and re-applies entries in place,
   * so existing references to this VFS see the updated state.
   */
  applySnapshot(entries: VfsSnapshotEntry[]): void {
    if (!entries || entries.length === 0) return;
    // Reset internal state
    this._inodes = new Map();
    this._nextIno = 1;
    const rootIno = this._allocInode(INODE_DIR, DEFAULT_DIR_MODE);
    this._root = rootIno;
    this._initLayout();
    this._applyEntries(entries);
  }

  /**
   * Apply snapshot entries to this VFS instance.
   */
  private _applyEntries(entries: VfsSnapshotEntry[]): void {
    for (const entry of entries) {
      if (entry.type === 'dir') {
        if (!this.exists(entry.path)) {
          this.mkdirp(entry.path);
        }
        if (entry.mode !== undefined) {
          this.chmod(entry.path, entry.mode);
        }
      } else if (entry.type === 'file') {
        const lastSlash = entry.path.lastIndexOf('/');
        const parent = lastSlash <= 0 ? '/' : entry.path.substring(0, lastSlash);
        if (!this.exists(parent)) {
          this.mkdirp(parent);
        }
        this.writeFile(entry.path, entry.data!);
        if (entry.mode !== undefined) {
          this.chmod(entry.path, entry.mode);
        }
      } else if (entry.type === 'symlink') {
        const lastSlash = entry.path.lastIndexOf('/');
        const parent = lastSlash <= 0 ? '/' : entry.path.substring(0, lastSlash);
        if (!this.exists(parent)) {
          this.mkdirp(parent);
        }
        if (!this.exists(entry.path)) {
          this.symlink(entry.target!, entry.path);
        }
      }
    }
  }
}
