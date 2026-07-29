/**
 * Integration test for WasmVM cooperative signal handling.
 *
 * Spawns the signal_handler C program as WASM (sigaction(SIGINT, ...) →
 * busy-loop with sleep → verify handler called), delivers SIGINT via
 * kernel.kill(), and verifies the handler fires at a syscall boundary.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agentos-vm-test-harness';
import {
  COMMANDS_DIR,
  C_BUILD_DIR,
  createKernel,
  describeIf,
  hasWasmBinaries,
  SIGTERM,
} from '@rivet-dev/agentos-vm-test-harness';
import type { Kernel } from '@rivet-dev/agentos-vm-test-harness';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

const hasCWasmBinaries = existsSync(join(C_BUILD_DIR, 'signal_handler'));
const SIGINT = 2;

function skipReason(): string | false {
  if (!hasWasmBinaries) return 'WASM binaries not built (run make wasm in native/wasmvm/)';
  if (!hasCWasmBinaries) return 'signal_handler WASM binary not built (run make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs)';
  return false;
}

// Minimal in-memory VFS
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
  lstat(path: string) { return this.stat(path); }
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

async function waitForSignalRegistration(
  kernel: Kernel,
  pid: number,
  signal: number,
): Promise<{ mask: Set<number>; flags: number }> {
  const proxy = (kernel as any).proxy;
  const entry = proxy?.trackedProcesses?.get(pid);
  let lastDirectKeys: number[] = [];
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const cached = kernel.processTable.getSignalState(pid).handlers.get(signal);
    if (cached) {
      return cached as { mask: Set<number>; flags: number };
    }

    if (proxy && entry) {
      const snapshot = await proxy.client.getSignalState(proxy.session, proxy.vm, entry.processId);
      lastDirectKeys = [...snapshot.handlers.keys()];
      const direct = snapshot?.handlers?.get(signal);
      if (direct) {
        return {
          mask: new Set(direct.mask),
          flags: direct.flags,
        };
      }
    }

    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(
    `timed out waiting for signal ${signal} registration for pid ${pid}; processId=${entry?.processId ?? 'unknown'}; sidecarSignals=${JSON.stringify(lastDirectKeys)}`,
  );
}

async function waitForOutput(
  getOutput: () => string,
  needle: string,
  timeoutMs = 2_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline && !getOutput().includes(needle)) {
    await new Promise((r) => setTimeout(r, 20));
  }
}

describeIf(!skipReason(), 'WasmVM signal handler integration', { timeout: 30_000 }, () => {
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

  it('signal_handler: sigaction registration preserves mask and fires at syscall boundary', async () => {
    // Spawn the WASM signal_handler program (registers SIGINT handler, then loops)
    let stdout = '';
    let stderr = '';
    const proc = kernel.spawn('signal_handler', [], {
      onStdout: (data) => { stdout += new TextDecoder().decode(data); },
      onStderr: (data) => { stderr += new TextDecoder().decode(data); },
    });

    // Wait for the program to register its handler and start waiting
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline && !stdout.includes('waiting')) {
      await new Promise((r) => setTimeout(r, 20));
    }
    expect(stdout).toContain('handler_registered');
    expect(stdout).toContain('waiting');

    const registration = await waitForSignalRegistration(kernel, proc.pid, SIGINT).catch((error) => {
      throw new Error(`${error instanceof Error ? error.message : String(error)}; stderr=${JSON.stringify(stderr)}`);
    });
    expect(registration?.mask).toEqual(new Set([SIGTERM]));

    // Deliver SIGINT via ManagedProcess.kill() — routes through kernel process table
    proc.kill(SIGINT);

    // Wait for the program to handle the signal and exit
    const exitCode = await proc.wait();
    await waitForOutput(() => stdout, 'caught_signal=2');

    if (!stdout.includes('caught_signal=2')) {
      throw new Error(`missing caught signal output; exitCode=${exitCode}; stdout=${JSON.stringify(stdout)}; stderr=${JSON.stringify(stderr)}`);
    }
    expect(exitCode).toBe(0);
  });
});
