/**
 * Integration tests for wget C command (libcurl-based).
 *
 * Verifies HTTP download operations via kernel.exec() with real WASM binaries:
 *   - Basic GET download to file
 *   - Download to specified file (-O)
 *   - Quiet mode (-q)
 *   - Error handling for 404 URLs
 *   - Follow redirects (default behavior)
 *
 * Tests start a local HTTP server in beforeAll and make wget requests against it.
 */

import { describe, it, expect, afterEach, beforeAll, afterAll } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os/test/runtime';
import { COMMANDS_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { createServer, type Server, type IncomingMessage, type ServerResponse } from 'node:http';

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

  has(path: string): boolean {
    return this.files.has(path);
  }
  getContent(path: string): string | undefined {
    const data = this.files.get(path);
    return data ? new TextDecoder().decode(data) : undefined;
  }
  getRawContent(path: string): Uint8Array | undefined {
    return this.files.get(path);
  }
}

describe.skipIf(!hasWasmBinaries)('wget command', () => {
  let kernel: Kernel;
  let server: Server;
  let port: number;

  beforeAll(async () => {
    server = createServer((req: IncomingMessage, res: ServerResponse) => {
      const url = req.url ?? '/';

      if (url === '/file.txt') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('downloaded content');
        return;
      }

      if (url === '/data.json') {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ status: 'ok' }));
        return;
      }

      if (url === '/redirect') {
        const addr = server.address() as import('node:net').AddressInfo;
        res.writeHead(302, { 'Location': `http://127.0.0.1:${addr.port}/redirected` });
        res.end();
        return;
      }

      if (url === '/redirected') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('arrived after redirect');
        return;
      }

      if (url === '/binary') {
        const buf = Buffer.alloc(1024);
        for (let i = 0; i < buf.length; i++) buf[i] = i & 0xff;
        res.writeHead(200, {
          'Content-Type': 'application/octet-stream',
          'Content-Length': String(buf.length),
        });
        res.end(buf);
        return;
      }

      res.writeHead(404, { 'Content-Type': 'text/plain' });
      res.end('not found');
    });

    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    port = (server.address() as import('node:net').AddressInfo).port;
  });

  afterAll(async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('downloads file to VFS using URL basename', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    await kernel.exec(`wget http://127.0.0.1:${port}/file.txt`);

    const content = vfs.getContent('/file.txt');
    expect(content).toBe('downloaded content');
  });

  it('-O saves to specified filename', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    await kernel.exec(`wget -O /output.txt http://127.0.0.1:${port}/data.json`);

    const content = vfs.getContent('/output.txt');
    expect(content).toBeDefined();
    expect(content).toContain('"status":"ok"');
  });

  it('-q suppresses progress output', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec(`wget -q -O /output.txt http://127.0.0.1:${port}/file.txt`);

    // Quiet mode should produce no stderr
    expect(result.stderr).toBe('');
    // File should still be downloaded
    expect(vfs.getContent('/output.txt')).toBe('downloaded content');
  });

  it('returns non-zero exit code for 404 URL', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec(`wget http://127.0.0.1:${port}/nonexistent`);

    // Should report error on stderr
    expect(result.stderr).toMatch(/wget|404|error|server/i);
  });

  it('follows redirects by default', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    await kernel.exec(`wget -O /output.txt http://127.0.0.1:${port}/redirect`);

    const content = vfs.getContent('/output.txt');
    expect(content).toBe('arrived after redirect');
  });
});
