/**
 * Integration test for WasmVM TCP server sockets.
 *
 * Spawns the tcp_server C program as WASM (bind → listen → accept → recv →
 * send "pong" → close), connects to it from the kernel as a client socket,
 * and verifies the full data exchange via loopback routing.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import {
  AF_INET,
  COMMANDS_DIR,
  C_BUILD_DIR,
  createKernel,
  describeIf,
  hasWasmBinaries,
  SOCK_STREAM,
} from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

const hasCWasmBinaries = existsSync(join(C_BUILD_DIR, 'tcp_server'));

function skipReason(): string | false {
  if (!hasWasmBinaries) return 'WASM binaries not built (run make wasm in native/wasmvm/)';
  if (!hasCWasmBinaries) return 'tcp_server WASM binary not built (run make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs)';
  return false;
}

// Minimal in-memory VFS (same as c-parity)
class SimpleVFS {
  private files = new Map<string, Uint8Array>();
  private dirs = new Set<string>(['/']);
  private symlinks = new Map<string, string>();

  async readFile(path: string): Promise<Uint8Array> {
    const data = this.files.get(path);
    if (!data) throw new Error(`ENOENT: ${path}`);
    return data;
  }
  async readTextFile(path: string): Promise<string> {
    return new TextDecoder().decode(await this.readFile(path));
  }
  async pread(path: string, offset: number, length: number): Promise<Uint8Array> {
    const data = await this.readFile(path);
    return data.slice(offset, offset + length);
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
    return (await this.readDir(path)).map((name) => ({
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
  async mkdir(path: string, _options?: { recursive?: boolean }) { this.dirs.add(path); }
  async exists(path: string): Promise<boolean> {
    return this.files.has(path) || this.dirs.has(path) || this.symlinks.has(path);
  }
  async stat(path: string) {
    const isDir = this.dirs.has(path);
    const isSymlink = this.symlinks.has(path);
    const data = this.files.get(path);
    if (!isDir && !isSymlink && !data) throw new Error(`ENOENT: ${path}`);
    return {
      mode: isSymlink ? 0o120777 : (isDir ? 0o40755 : 0o100644),
      size: data?.length ?? 0,
      isDirectory: isDir,
      isSymbolicLink: isSymlink,
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
  async chmod() {}
  async rename(from: string, to: string) {
    const data = this.files.get(from);
    if (data) { this.files.set(to, data); this.files.delete(from); }
  }
  async unlink(path: string) { this.files.delete(path); this.symlinks.delete(path); }
  async rmdir(path: string) { this.dirs.delete(path); }
  async symlink(target: string, linkPath: string) {
    this.symlinks.set(linkPath, target);
    const parts = linkPath.split('/').filter(Boolean);
    for (let i = 1; i < parts.length; i++) {
      this.dirs.add('/' + parts.slice(0, i).join('/'));
    }
  }
  async readlink(path: string): Promise<string> {
    const target = this.symlinks.get(path);
    if (!target) throw new Error(`EINVAL: ${path}`);
    return target;
  }
}

// Wait for a kernel socket listener on the given port (poll with timeout)
async function waitForListener(
  kernel: Kernel,
  port: number,
  timeoutMs = 10_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const listener = kernel.socketTable.findListener({ host: '0.0.0.0', port });
    if (listener) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`Timed out waiting for listener on port ${port}`);
}

const TEST_PORT = 9876;
const CLIENT_PID = 999; // Fake PID for test-side client sockets

describeIf(!skipReason(), 'WasmVM TCP server integration', { timeout: 30_000 }, () => {
  let kernel: Kernel;
  let vfs: SimpleVFS;

  beforeEach(async () => {
    vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }));
  });

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('tcp_server: accept connection, recv data, send pong', async () => {
    // Start the WASM TCP server (blocks on accept until we connect)
    const execPromise = kernel.exec(`tcp_server ${TEST_PORT}`);

    // Wait for the server to finish bind+listen
    await waitForListener(kernel, TEST_PORT);

    // Create a client socket and connect via loopback
    const st = kernel.socketTable;
    const clientId = st.create(AF_INET, SOCK_STREAM, 0, CLIENT_PID);
    await st.connect(clientId, { host: '127.0.0.1', port: TEST_PORT });

    // Send "ping" to the server
    const encoder = new TextEncoder();
    st.send(clientId, encoder.encode('ping'));

    // Wait for the server to process and send its reply
    const decoder = new TextDecoder();
    let reply = '';
    const recvDeadline = Date.now() + 10_000;
    while (Date.now() < recvDeadline) {
      const chunk = st.recv(clientId, 256);
      if (chunk && chunk.length > 0) {
        reply += decoder.decode(chunk);
        break;
      }
      // No data yet — yield to let the WASM worker process
      await new Promise((r) => setTimeout(r, 20));
    }

    expect(reply).toBe('pong');

    // Close client socket
    st.close(clientId, CLIENT_PID);

    // Wait for exec to complete (server exits after handling one connection)
    const result = await execPromise;

    expect(result.stdout).toContain('listening on port 9876');
    expect(result.stdout).toContain('received: ping');
    expect(result.stdout).toContain('sent: 4');
    expect(result.exitCode).toBe(0);
  });
});
