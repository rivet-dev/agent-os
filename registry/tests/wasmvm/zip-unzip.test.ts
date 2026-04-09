/**
 * Integration tests for zip and unzip C commands.
 *
 * Verifies zip/unzip roundtrip, recursive compression, list mode,
 * and extract-to-directory via kernel.exec() with real WASM binaries.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import { COMMANDS_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
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
    // Ensure parent dirs exist
    const parts = path.split('/').filter(Boolean);
    for (let i = 1; i < parts.length; i++) {
      this.dirs.add('/' + parts.slice(0, i).join('/'));
    }
  }
  async createDir(path: string) { this.dirs.add(path); }
  async mkdir(path: string, _options?: { recursive?: boolean }) {
    this.dirs.add(path);
    // Also create parent dirs
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

describe.skipIf(!hasWasmBinaries)('zip/unzip commands', () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('zip creates valid archive, unzip extracts it, contents match', async () => {
    const vfs = new SimpleVFS();
    await vfs.writeFile('/hello.txt', 'Hello, World!\n');

    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    // Create zip archive
    const zipResult = await kernel.exec('zip /archive.zip /hello.txt');
    expect(zipResult.exitCode).toBe(0);

    // Verify archive was created
    expect(await vfs.exists('/archive.zip')).toBe(true);

    // Extract to a different directory
    const unzipResult = await kernel.exec('unzip -d /extracted /archive.zip');
    expect(unzipResult.exitCode).toBe(0);

    // Verify extracted content matches original
    const extracted = await vfs.readTextFile('/extracted/hello.txt');
    expect(extracted).toBe('Hello, World!\n');
  });

  it('zip -r compresses directory recursively', async () => {
    const vfs = new SimpleVFS();
    await vfs.mkdir('/mydir');
    await vfs.writeFile('/mydir/a.txt', 'file a\n');
    await vfs.writeFile('/mydir/b.txt', 'file b\n');

    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const zipResult = await kernel.exec('zip -r /dir.zip /mydir');
    expect(zipResult.exitCode).toBe(0);
    expect(await vfs.exists('/dir.zip')).toBe(true);

    // Extract and verify
    const unzipResult = await kernel.exec('unzip -d /out /dir.zip');
    expect(unzipResult.exitCode).toBe(0);

    const a = await vfs.readTextFile('/out/mydir/a.txt');
    const b = await vfs.readTextFile('/out/mydir/b.txt');
    expect(a).toBe('file a\n');
    expect(b).toBe('file b\n');
  });

  it('unzip -l lists archive contents with sizes', async () => {
    const vfs = new SimpleVFS();
    await vfs.writeFile('/data.txt', 'some data content\n');

    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    // Create archive first
    const zipResult = await kernel.exec('zip /list-test.zip /data.txt');
    expect(zipResult.exitCode).toBe(0);

    // List contents
    const listResult = await kernel.exec('unzip -l /list-test.zip');
    expect(listResult.exitCode).toBe(0);
    expect(listResult.stdout).toContain('data.txt');
    // Should show the file size (18 bytes)
    expect(listResult.stdout).toContain('18');
    // Should show summary line with file count
    expect(listResult.stdout).toMatch(/1 file/);
  });

  it('zip/unzip roundtrip preserves file contents exactly', async () => {
    const vfs = new SimpleVFS();
    // Binary-like content with various byte values
    const content = new Uint8Array(256);
    for (let i = 0; i < 256; i++) content[i] = i;
    await vfs.writeFile('/binary.bin', content);

    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const zipResult = await kernel.exec('zip /roundtrip.zip /binary.bin');
    expect(zipResult.exitCode).toBe(0);

    const unzipResult = await kernel.exec('unzip -d /rt-out /roundtrip.zip');
    expect(unzipResult.exitCode).toBe(0);

    const extracted = await vfs.readFile('/rt-out/binary.bin');
    expect(extracted.length).toBe(256);
    for (let i = 0; i < 256; i++) {
      expect(extracted[i]).toBe(i);
    }
  });

  it('unzip -d extracts to specified directory', async () => {
    const vfs = new SimpleVFS();
    await vfs.writeFile('/src.txt', 'target content\n');

    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const zipResult = await kernel.exec('zip /dest-test.zip /src.txt');
    expect(zipResult.exitCode).toBe(0);

    // Extract to a new directory
    const unzipResult = await kernel.exec('unzip -d /custom-dir /dest-test.zip');
    expect(unzipResult.exitCode).toBe(0);

    expect(await vfs.exists('/custom-dir/src.txt')).toBe(true);
    const extracted = await vfs.readTextFile('/custom-dir/src.txt');
    expect(extracted).toBe('target content\n');
  });
});
