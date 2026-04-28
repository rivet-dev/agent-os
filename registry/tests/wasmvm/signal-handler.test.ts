/**
 * Integration test for WasmVM cooperative signal handling.
 *
 * Spawns the signal_handler C program as WASM (sigaction(SIGINT, ...) →
 * busy-loop with sleep → verify handler called), delivers SIGINT via
 * kernel.kill(), and verifies the handler fires at a syscall boundary.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import {
  COMMANDS_DIR,
  C_BUILD_DIR,
  createKernel,
  describeIf,
  hasWasmBinaries,
  SIGTERM,
} from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

const hasCWasmBinaries = existsSync(join(C_BUILD_DIR, 'signal_handler'));
const EXPECTED_SIGACTION_FLAGS = (0x10000000 | 0x80000000) >>> 0;

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

  it('signal_handler: sigaction registration preserves mask/flags and fires at syscall boundary', async () => {
    // Spawn the WASM signal_handler program (registers SIGINT handler, then loops)
    let stdout = '';
    const proc = kernel.spawn('signal_handler', [], {
      onStdout: (data) => { stdout += new TextDecoder().decode(data); },
    });

    // Wait for the program to register its handler and start waiting
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline && !stdout.includes('waiting')) {
      await new Promise((r) => setTimeout(r, 20));
    }
    expect(stdout).toContain('handler_registered');
    expect(stdout).toContain('waiting');

    const registration = kernel.processTable.getSignalState(proc.pid).handlers.get(2);
    expect(registration?.mask).toEqual(new Set([SIGTERM]));
    expect(registration?.flags).toBe(EXPECTED_SIGACTION_FLAGS);

    // Deliver SIGINT via ManagedProcess.kill() — routes through kernel process table
    proc.kill(2 /* SIGINT */);

    // Wait for the program to handle the signal and exit
    const exitCode = await proc.wait();

    expect(stdout).toContain('caught_signal=2');
    expect(exitCode).toBe(0);
  });
});
