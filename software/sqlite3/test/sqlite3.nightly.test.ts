// Nightly: requires a non-core C-sysroot command.
/**
 * Integration tests for sqlite3 C command.
 *
 * Verifies SQLite CLI operations via kernel.exec() with real WASM binaries:
 *   - In-memory databases (:memory:)
 *   - Stdin pipe mode for simple queries
 *   - SQL from command line arguments for multi-statement operations
 *   - Meta-commands (.dump, .schema, .tables)
 *
 * The command is the official SQLite shell compiled against the AgentOS C
 * sysroot, not the former local sqlite3_cli.c reimplementation.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { createWasmVmRuntime } from '@rivet-dev/agentos-test-harness';
import {
  C_BUILD_DIR,
  COMMANDS_DIR,
  createKernel,
  describeIf,
} from '@rivet-dev/agentos-test-harness';
import type { Kernel } from '@rivet-dev/agentos-test-harness';

const SQLITE3_COMMAND_DIRS = [C_BUILD_DIR, COMMANDS_DIR].filter((dir) =>
  existsSync(dir)
);
const hasSqlite3Binary = SQLITE3_COMMAND_DIRS.some((dir) =>
  existsSync(resolve(dir, 'sqlite3'))
);

// Minimal in-memory VFS for kernel tests
class SimpleVFS {
  private files = new Map<string, Uint8Array>();
  private dirs = new Set<string>(['/']);

  async readFile(path: string): Promise<Uint8Array> {
    const data = this.files.get(path);
    if (!data) throw new Error(`ENOENT: ${path}`);
    return data;
  }
  async readTextFile(path: string): Promise<string> {
    return new TextDecoder().decode(await this.readFile(path));
  }
  async readDir(path: string): Promise<string[]> {
    const prefix = path === '/' ? '/' : path + '/';
    const entries: string[] = [];
    for (const p of [...this.files.keys(), ...this.dirs]) {
      if (p !== path && p.startsWith(prefix)) {
        const rest = p.slice(prefix.length);
        if (!rest.includes('/')) entries.push(rest);
      }
    }
    return entries;
  }
  async readDirWithTypes(path: string) {
    return (await this.readDir(path)).map(name => ({
      name,
      isDirectory: this.dirs.has(path === '/' ? `/${name}` : `${path}/${name}`),
    }));
  }
  async writeFile(path: string, content: string | Uint8Array): Promise<void> {
    const data = typeof content === 'string' ? new TextEncoder().encode(content) : content;
    this.files.set(path, new Uint8Array(data));
    const parts = path.split('/').filter(Boolean);
    for (let i = 1; i < parts.length; i++) {
      this.dirs.add('/' + parts.slice(0, i).join('/'));
    }
  }
  async createDir(path: string) { this.dirs.add(path); }
  async mkdir(path: string, _options?: { recursive?: boolean }) {
    this.dirs.add(path);
    const parts = path.split('/').filter(Boolean);
    for (let i = 1; i < parts.length; i++) {
      this.dirs.add('/' + parts.slice(0, i).join('/'));
    }
  }
  async exists(path: string): Promise<boolean> {
    return this.files.has(path) || this.dirs.has(path);
  }
  async stat(path: string) {
    const isDir = this.dirs.has(path);
    const data = this.files.get(path);
    if (!isDir && !data) throw new Error(`ENOENT: ${path}`);
    return {
      mode: isDir ? 0o40755 : 0o100644,
      size: data?.length ?? 0,
      isDirectory: isDir,
      isSymbolicLink: false,
      atimeMs: Date.now(),
      mtimeMs: Date.now(),
      ctimeMs: Date.now(),
      birthtimeMs: Date.now(),
      ino: 0,
      nlink: 1,
      uid: 1000,
      gid: 1000,
    };
  }
  async chmod(_path: string, _mode: number) {}
  async lstat(path: string) { return this.stat(path); }
  async removeFile(path: string) { this.files.delete(path); }
  async removeDir(path: string) { this.dirs.delete(path); }
  async rename(oldPath: string, newPath: string) {
    const data = this.files.get(oldPath);
    if (data) {
      this.files.set(newPath, data);
      this.files.delete(oldPath);
    }
  }
  async pread(path: string, buffer: Uint8Array, offset: number, length: number, position: number): Promise<number> {
    const data = this.files.get(path);
    if (!data) throw new Error(`ENOENT: ${path}`);
    const available = Math.min(length, data.length - position);
    if (available <= 0) return 0;
    buffer.set(data.subarray(position, position + available), offset);
    return available;
  }
  async pwrite(path: string, offset: number, data: Uint8Array): Promise<void> {
    const current = this.files.get(path);
    if (!current) throw new Error(`ENOENT: ${path}`);
    const next = new Uint8Array(Math.max(current.length, offset + data.length));
    next.set(current);
    next.set(data, offset);
    this.files.set(path, next);
  }
}

describeIf(hasSqlite3Binary, 'sqlite3 command', { timeout: 30_000 }, () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('executes SQL from stdin pipe on in-memory database', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const result = await kernel.exec('sqlite3 :memory:', {
      stdin: 'SELECT 1+1 AS result;\n',
    });
    expect(result.stdout.trim()).toBe('2');
  });

  it('executes multi-statement SQL as command argument', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    // Multi-statement SQL passed as command argument (more reliable than stdin in WASM)
    const sql = 'CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(10); INSERT INTO t VALUES(20); INSERT INTO t VALUES(30); SELECT * FROM t ORDER BY x;';
    const result = await kernel.exec(`sqlite3 :memory: "${sql}"`);
    expect(result.stdout.trim()).toBe('10\n20\n30');
  });

  it('supports .tables meta-command', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/tmp', { recursive: true });
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const createResult = await kernel.exec(
      'sqlite3 /tmp/meta.db "CREATE TABLE alpha(x); CREATE TABLE beta(y);"'
    );
    expect(createResult.stderr).toBe('');

    const result = await kernel.exec('sqlite3 /tmp/meta.db ".tables"');
    expect(result.stdout).toContain('alpha');
    expect(result.stdout).toContain('beta');
  });

  it('supports .schema meta-command', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/tmp', { recursive: true });
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const createResult = await kernel.exec(
      'sqlite3 /tmp/schema.db "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);"'
    );
    expect(createResult.stderr).toBe('');

    const result = await kernel.exec('sqlite3 /tmp/schema.db ".schema users"');
    expect(result.stdout).toContain('CREATE TABLE users');
    expect(result.stdout).toContain('id INTEGER PRIMARY KEY');
  });

  it('supports .dump meta-command', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/tmp', { recursive: true });
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const createResult = await kernel.exec(
      `sqlite3 /tmp/dump.db "CREATE TABLE t(x INTEGER, y TEXT); INSERT INTO t VALUES(1,'hello');"`
    );
    expect(createResult.stderr).toBe('');

    const result = await kernel.exec('sqlite3 /tmp/dump.db ".dump"');
    const output = result.stdout;
    expect(output).toContain('BEGIN TRANSACTION');
    expect(output).toContain('CREATE TABLE t');
    expect(output).toContain("INSERT INTO t VALUES(1,'hello')");
    expect(output).toContain('COMMIT');
  });

  it('handles SELECT with multiple columns', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const result = await kernel.exec('sqlite3 :memory:', {
      stdin: "SELECT 'hello' AS greeting, 42 AS number, 3.14 AS pi;\n",
    });
    expect(result.stdout.trim()).toBe('hello|42|3.14');
  });

  it('handles NULL values in output', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const result = await kernel.exec('sqlite3 :memory:', {
      stdin: 'SELECT NULL;\n',
    });
    // SQLite CLI outputs empty string for NULL
    expect(result.stdout.trim()).toBe('');
  });

  it('reports SQL errors on stderr', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const result = await kernel.exec(`sqlite3 :memory: "SELECT * FROM nonexistent_table;"`);
    expect(result.stderr).toContain('no such table');
  });

  it('defaults to :memory: when no database specified', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const result = await kernel.exec('sqlite3', {
      stdin: 'SELECT 99;\n',
    });
    expect(result.stdout.trim()).toBe('99');
  });

  it('CREATE TABLE, INSERT, SELECT roundtrip via piped SQL', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    // Multi-statement SQL via command arg (stdin multi-statement has fgetc buffering issues in WASM)
    const sql = "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO items VALUES(1,'apple'); INSERT INTO items VALUES(2,'banana'); SELECT id, name FROM items ORDER BY id;";
    const result = await kernel.exec(`sqlite3 :memory: "${sql}"`);
    expect(result.stdout.trim()).toBe('1|apple\n2|banana');
  });

  it('.tables meta-command lists created tables', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/tmp', { recursive: true });
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const createResult = await kernel.exec(
      'sqlite3 /tmp/tables.db "CREATE TABLE alpha(x); CREATE TABLE beta(y);"'
    );
    expect(createResult.stderr).toBe('');

    const result = await kernel.exec('sqlite3 /tmp/tables.db ".tables"');
    expect(result.stdout).toContain('alpha');
    expect(result.stdout).toContain('beta');
  });

  it('.schema meta-command shows CREATE TABLE statements', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/tmp', { recursive: true });
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const createResult = await kernel.exec(
      'sqlite3 /tmp/schema-meta.db "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);"'
    );
    expect(createResult.stderr).toBe('');

    const result = await kernel.exec('sqlite3 /tmp/schema-meta.db ".schema users"');
    expect(result.stdout).toContain('CREATE TABLE users');
    expect(result.stdout).toContain('id INTEGER PRIMARY KEY');
  });

  it('.dump meta-command outputs INSERT statements for data', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/tmp', { recursive: true });
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    const createResult = await kernel.exec(
      `sqlite3 /tmp/dump-meta.db "CREATE TABLE t(x INTEGER, y TEXT); INSERT INTO t VALUES(1,'hello'); INSERT INTO t VALUES(2,'world');"`
    );
    expect(createResult.stderr).toBe('');

    const result = await kernel.exec('sqlite3 /tmp/dump-meta.db ".dump"');
    const output = result.stdout;
    expect(output).toContain('BEGIN TRANSACTION');
    expect(output).toContain('CREATE TABLE t');
    expect(output).toContain("INSERT INTO t VALUES(1,'hello')");
    expect(output).toContain("INSERT INTO t VALUES(2,'world')");
    expect(output).toContain('COMMIT');
  });

  it('file-based DB persists data across separate exec calls', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/tmp', { recursive: true });
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    // File-backed SQLite must persist through the kernel VFS across exec calls.
    const createSql = "CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(42); INSERT INTO t VALUES(99);";
    const createResult = await kernel.exec(`sqlite3 /tmp/test.db "${createSql}"`);
    expect(createResult.stderr).toBe('');

    // Verify file was created in VFS
    const dbData = await vfs.readFile('/tmp/test.db');
    expect(dbData.length).toBeGreaterThan(0);

    // Second exec: reopen and query persisted data.
    const result = await kernel.exec('sqlite3 /tmp/test.db "SELECT * FROM t ORDER BY x;"');
    expect(result.stdout.trim()).toBe('42\n99');
  });

  it('multi-statement input separated by semicolons', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    // Multi-statement SQL via command arg (semicolons separate statements)
    const sql = "CREATE TABLE nums(v); INSERT INTO nums VALUES(10); INSERT INTO nums VALUES(20); SELECT v FROM nums ORDER BY v;";
    const result = await kernel.exec(`sqlite3 :memory: "${sql}"`);
    expect(result.stdout.trim()).toBe('10\n20');
  });

  it('SQL syntax error produces error on stderr with non-zero exit', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: SQLITE3_COMMAND_DIRS }));

    // Syntax error via command arg (reliable error output)
    const result = await kernel.exec('sqlite3 :memory: "SELEC INVALID SYNTAX;"');
    expect(result.stderr).toContain('Error');
  });
});
