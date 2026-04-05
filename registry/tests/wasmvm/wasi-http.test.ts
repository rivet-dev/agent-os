/**
 * Integration tests for wasi-http Rust library (HTTP/1.1 client via host_net).
 *
 * Verifies HTTP client functionality through the http-test WASM binary:
 *   - GET request with response body
 *   - POST request with JSON body
 *   - Custom headers
 *   - HTTPS via TLS upgrade
 *   - SSE (Server-Sent Events) streaming
 *
 * Tests start local HTTP/HTTPS servers and run http-test via kernel.exec().
 */

import { describe, it, expect, afterEach, beforeAll, afterAll } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os/test/runtime';
import { COMMANDS_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { createServer as createHttpServer, type Server, type IncomingMessage, type ServerResponse } from 'node:http';
import { createServer as createHttpsServer, type Server as HttpsServer } from 'node:https';
import { execSync } from 'node:child_process';
import { existsSync } from 'node:fs';

// Check if openssl CLI is available for generating test certs
let hasOpenssl = false;
try {
  execSync('openssl version', { stdio: 'pipe' });
  hasOpenssl = true;
} catch { /* openssl not available */ }

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
}

// HTTP request handler
function requestHandler(port: number) {
  return (req: IncomingMessage, res: ServerResponse) => {
    const url = req.url ?? '/';

    // GET / — basic response
    if (url === '/' && req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      res.end('hello from wasi-http test');
      return;
    }

    // GET /json — JSON response
    if (url === '/json' && req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ status: 'ok', message: 'json response' }));
      return;
    }

    // POST /echo-body — echo JSON body back
    if (url === '/echo-body' && req.method === 'POST') {
      let body = '';
      req.on('data', (chunk: Buffer) => { body += chunk.toString(); });
      req.on('end', () => {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ received: body, contentType: req.headers['content-type'] }));
      });
      return;
    }

    // GET /echo-headers — echo back request headers
    if (url === '/echo-headers') {
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      const xCustom = req.headers['x-custom-header'] ?? 'none';
      const xAnother = req.headers['x-another'] ?? 'none';
      res.end(`x-custom-header: ${xCustom}\nx-another: ${xAnother}`);
      return;
    }

    // GET /sse — SSE stream with 3 events
    if (url === '/sse') {
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        'Cache-Control': 'no-cache',
        'Connection': 'close',
      });
      res.write('event: message\ndata: hello\n\n');
      res.write('event: update\ndata: world\nid: 1\n\n');
      res.write('data: done\n\n');
      res.end();
      return;
    }

    res.writeHead(404, { 'Content-Type': 'text/plain' });
    res.end('not found');
  };
}

describe.skipIf(!hasWasmBinaries)('wasi-http client (http-test binary)', () => {
  let kernel: Kernel;
  let server: Server;
  let port: number;

  beforeAll(async () => {
    server = createHttpServer(requestHandler(0));
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    port = (server.address() as import('node:net').AddressInfo).port;
    // Patch handler to use actual port
    server.removeAllListeners('request');
    server.on('request', requestHandler(port));
  });

  afterAll(async () => {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('GET returns status and body', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec(`http-test get http://127.0.0.1:${port}/`);
    expect(result.stdout).toContain('status: 200');
    expect(result.stdout).toContain('body: hello from wasi-http test');
  });

  it('GET returns JSON response', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec(`http-test get http://127.0.0.1:${port}/json`);
    expect(result.stdout).toContain('status: 200');
    expect(result.stdout).toContain('"status":"ok"');
  });

  it('POST sends JSON body correctly', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const jsonBody = '{"key":"value","num":42}';
    const result = await kernel.exec(`http-test post http://127.0.0.1:${port}/echo-body '${jsonBody}'`);
    expect(result.stdout).toContain('status: 200');
    // Verify server received the JSON body and content-type
    expect(result.stdout).toContain('"received":"{\\"key\\":\\"value\\",\\"num\\":42}"');
    expect(result.stdout).toContain('application/json');
  });

  it('GET with custom headers sends headers correctly', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec(
      `http-test headers http://127.0.0.1:${port}/echo-headers 'X-Custom-Header:test-value' 'X-Another:second'`
    );
    expect(result.stdout).toContain('status: 200');
    expect(result.stdout).toContain('x-custom-header: test-value');
    expect(result.stdout).toContain('x-another: second');
  });

  it('SSE streaming receives events', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec(`http-test sse http://127.0.0.1:${port}/sse`);
    expect(result.stdout).toContain('status: 200');
    expect(result.stdout).toContain('event: message');
    expect(result.stdout).toContain('data: hello');
    expect(result.stdout).toContain('event: update');
    expect(result.stdout).toContain('data: world');
    expect(result.stdout).toContain('data: done');
  });

  it('GET to non-existent path returns 404', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec(`http-test get http://127.0.0.1:${port}/nonexistent`);
    expect(result.stdout).toContain('status: 404');
  });
});

describe.skipIf(!hasWasmBinaries || !hasOpenssl)('wasi-http HTTPS (http-test binary)', () => {
  let kernel: Kernel;
  let httpsServer: HttpsServer;
  let httpsPort: number;
  let certKey: string;
  let certPem: string;

  beforeAll(async () => {
    // Generate self-signed cert for testing
    const certResult = execSync(
      'openssl req -x509 -newkey rsa:2048 -keyout /dev/stdout -out /dev/stdout -days 1 -nodes -subj "/CN=localhost" 2>/dev/null',
      { encoding: 'utf8' },
    );
    // Extract key and cert from combined output
    const keyMatch = certResult.match(/-----BEGIN PRIVATE KEY-----[\s\S]+?-----END PRIVATE KEY-----/);
    const certMatch = certResult.match(/-----BEGIN CERTIFICATE-----[\s\S]+?-----END CERTIFICATE-----/);
    certKey = keyMatch![0];
    certPem = certMatch![0];

    httpsServer = createHttpsServer({ key: certKey, cert: certPem }, (req, res) => {
      if (req.url === '/' && req.method === 'GET') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('hello from https');
        return;
      }
      res.writeHead(404);
      res.end('not found');
    });
    await new Promise<void>((resolve) => httpsServer.listen(0, '127.0.0.1', resolve));
    httpsPort = (httpsServer.address() as import('node:net').AddressInfo).port;
  });

  afterAll(async () => {
    await new Promise<void>((resolve) => httpsServer.close(() => resolve()));
  });

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('HTTPS GET via TLS upgrade returns response', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    // Disable TLS verification for self-signed cert in tests
    const origReject = process.env.NODE_TLS_REJECT_UNAUTHORIZED;
    process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';
    try {
      const result = await kernel.exec(`http-test get https://127.0.0.1:${httpsPort}/`);
      expect(result.stdout).toContain('status: 200');
      expect(result.stdout).toContain('body: hello from https');
    } finally {
      if (origReject === undefined) {
        delete process.env.NODE_TLS_REJECT_UNAUTHORIZED;
      } else {
        process.env.NODE_TLS_REJECT_UNAUTHORIZED = origReject;
      }
    }
  });
});
