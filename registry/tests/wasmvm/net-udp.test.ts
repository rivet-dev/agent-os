/**
 * Integration test for WasmVM UDP sockets.
 *
 * Spawns the udp_echo C program as WASM (bind → recvfrom → sendto echo → close),
 * sends datagrams from a kernel client socket, and verifies the echo response
 * and message boundary preservation.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os/test/runtime';
import {
  AF_INET,
  COMMANDS_DIR,
  C_BUILD_DIR,
  createKernel,
  hasWasmBinaries,
  SOCK_DGRAM,
} from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

const hasCWasmBinaries = existsSync(join(C_BUILD_DIR, 'udp_echo'));

function skipReason(): string | false {
  if (!hasWasmBinaries) return 'WASM binaries not built (run make wasm in native/wasmvm/)';
  if (!hasCWasmBinaries) return 'udp_echo WASM binary not built (run make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs)';
  return false;
}

// Minimal in-memory VFS (same as net-server)
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

// Wait for a kernel UDP binding on the given port (poll with timeout)
async function waitForUdpBinding(
  kernel: Kernel,
  port: number,
  timeoutMs = 10_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const bound = kernel.socketTable.findBoundUdp({ host: '0.0.0.0', port });
    if (bound) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`Timed out waiting for UDP binding on port ${port}`);
}

const TEST_PORT = 9877;
const CLIENT_PID = 999; // Fake PID for test-side client sockets

describe.skipIf(skipReason())('WasmVM UDP integration', { timeout: 30_000 }, () => {
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

  it('udp_echo: recv datagram and echo it back', async () => {
    // Start the WASM UDP echo server (blocks on recvfrom until we send)
    const execPromise = kernel.exec(`udp_echo ${TEST_PORT}`);

    // Wait for the server to finish bind
    await waitForUdpBinding(kernel, TEST_PORT);

    // Create a client UDP socket and bind to an ephemeral port
    const st = kernel.socketTable;
    const clientId = st.create(AF_INET, SOCK_DGRAM, 0, CLIENT_PID);
    await st.bind(clientId, { host: '127.0.0.1', port: 0 });

    // Send "hello" to the echo server
    const encoder = new TextEncoder();
    st.sendTo(clientId, encoder.encode('hello'), 0, { host: '127.0.0.1', port: TEST_PORT });

    // Wait for the echo response
    const decoder = new TextDecoder();
    let reply = '';
    const recvDeadline = Date.now() + 10_000;
    while (Date.now() < recvDeadline) {
      const result = st.recvFrom(clientId, 1024);
      if (result && result.data.length > 0) {
        reply = decoder.decode(result.data);
        break;
      }
      await new Promise((r) => setTimeout(r, 20));
    }

    expect(reply).toBe('hello');

    // Close client socket
    st.close(clientId, CLIENT_PID);

    // Wait for exec to complete (server exits after echoing one datagram)
    const result = await execPromise;

    expect(result.stdout).toContain('listening on port 9877');
    expect(result.stdout).toContain('received: hello');
    expect(result.stdout).toContain('echoed: 5');
    expect(result.exitCode).toBe(0);
  });

  it('udp_echo: message boundaries are preserved', async () => {
    // Start the WASM UDP echo server
    const execPromise = kernel.exec(`udp_echo ${TEST_PORT + 1}`);

    // Wait for the server to finish bind
    await waitForUdpBinding(kernel, TEST_PORT + 1);

    // Create a client UDP socket
    const st = kernel.socketTable;
    const clientId = st.create(AF_INET, SOCK_DGRAM, 0, CLIENT_PID);
    await st.bind(clientId, { host: '127.0.0.1', port: 0 });

    // Send a message — the echo server echoes exactly one datagram
    const encoder = new TextEncoder();
    const msg = 'boundary-test-message';
    st.sendTo(clientId, encoder.encode(msg), 0, { host: '127.0.0.1', port: TEST_PORT + 1 });

    // Receive the echo — it must be the exact message (not fragmented/merged)
    const decoder = new TextDecoder();
    let reply = '';
    const recvDeadline = Date.now() + 10_000;
    while (Date.now() < recvDeadline) {
      const result = st.recvFrom(clientId, 1024);
      if (result && result.data.length > 0) {
        reply = decoder.decode(result.data);
        break;
      }
      await new Promise((r) => setTimeout(r, 20));
    }

    // Message boundary preserved: exact content, exact length
    expect(reply).toBe(msg);
    expect(reply.length).toBe(msg.length);

    st.close(clientId, CLIENT_PID);
    const result = await execPromise;
    expect(result.exitCode).toBe(0);
  });
});
