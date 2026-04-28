/**
 * Integration tests for fd (fd-find) command.
 *
 * Verifies file finding with regex patterns, extension filters, type filters,
 * hidden file skipping, and empty directory handling via kernel.exec() with
 * real WASM binaries.
 *
 * Note: kernel.exec() wraps commands in sh -c. Brush-shell currently returns
 * exit code 17 for all child commands (benign "could not retrieve pid" issue).
 * Tests verify stdout correctness rather than exit code.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import { COMMANDS_DIR, createKernel, describeIf, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '../helpers.js';

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
}

/** Create a VFS pre-populated with a test directory structure */
async function createTestVFS(): Promise<SimpleVFS> {
  const vfs = new SimpleVFS();
  // /project/
  //   src/
  //     main.js
  //     utils.js
  //     helpers.ts
  //   lib/
  //     parser.js
  //   docs/
  //     readme.md
  //   .hidden/
  //     secret.txt
  //   .gitignore
  //   config.json
  await vfs.writeFile('/project/src/main.js', 'console.log("main")');
  await vfs.writeFile('/project/src/utils.js', 'export {}');
  await vfs.writeFile('/project/src/helpers.ts', 'export {}');
  await vfs.writeFile('/project/lib/parser.js', 'module.exports = {}');
  await vfs.writeFile('/project/docs/readme.md', '# Readme');
  await vfs.writeFile('/project/.hidden/secret.txt', 'secret');
  await vfs.writeFile('/project/.gitignore', 'node_modules');
  await vfs.writeFile('/project/config.json', '{}');
  // /empty/ — empty directory
  await vfs.mkdir('/empty', { recursive: true });
  return vfs;
}

/** Parse fd output lines, sorted for deterministic comparison */
function parseLines(stdout: string): string[] {
  return stdout.split('\n').filter(l => l.length > 0).sort();
}

describeIf(hasWasmBinaries, 'fd-find command', { timeout: 10_000 }, () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('finds files matching regex pattern in current directory', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd main /project', {});
    const lines = parseLines(result.stdout);
    expect(lines).toContain('/project/src/main.js');
  });

  it('finds all .js files with -e js', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -e js . /project', {});
    const lines = parseLines(result.stdout);
    expect(lines).toContain('/project/src/main.js');
    expect(lines).toContain('/project/src/utils.js');
    expect(lines).toContain('/project/lib/parser.js');
    // .ts files should NOT match
    expect(lines).not.toContain('/project/src/helpers.ts');
  });

  it('finds only files with -t f', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -t f . /project', {});
    const lines = parseLines(result.stdout);
    // All entries should be files, not directories
    for (const line of lines) {
      const stat = await vfs.stat(line);
      expect(stat.isDirectory).toBe(false);
    }
    // Should include known files
    expect(lines).toContain('/project/src/main.js');
    expect(lines).toContain('/project/config.json');
  });

  it('finds only directories with -t d', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -t d . /project', {});
    const lines = parseLines(result.stdout);
    // All entries should be directories
    for (const line of lines) {
      const stat = await vfs.stat(line);
      expect(stat.isDirectory).toBe(true);
    }
    // Should include known directories (hidden skipped by default)
    expect(lines).toContain('/project/src');
    expect(lines).toContain('/project/lib');
    expect(lines).toContain('/project/docs');
  });

  it('returns no results for empty directory', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd . /empty', {});
    expect(result.stdout.trim()).toBe('');
  });

  it('returns empty output when no files match pattern', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd zzzznonexistent /project', {});
    expect(result.stdout.trim()).toBe('');
  });

  it('skips hidden files and directories by default', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd . /project', {});
    const lines = parseLines(result.stdout);
    // Hidden files/dirs should NOT appear
    const hiddenEntries = lines.filter(l => {
      const parts = l.split('/');
      return parts.some(p => p.startsWith('.') && p.length > 1);
    });
    expect(hiddenEntries).toEqual([]);
  });

  it('includes hidden files with -H flag', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -H . /project', {});
    const lines = parseLines(result.stdout);
    // Hidden items should now appear
    expect(lines).toContain('/project/.gitignore');
    expect(lines).toContain('/project/.hidden');
  });
});
