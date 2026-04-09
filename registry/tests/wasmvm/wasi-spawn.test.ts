/**
 * Tests for wasi-spawn WasiChild — host_process FFI spawn with pipe capture.
 *
 * Exercises the spawn-test-host binary which uses the wasi-spawn library
 * to spawn child processes via host_process imports and capture output
 * through pipes.
 *
 * Requires WASM binaries built (make wasm in native/wasmvm/).
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import { COMMANDS_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';

function skipReason(): string | false {
  if (!hasWasmBinaries) return 'WASM binaries not built (run make wasm in native/wasmvm/)';
  if (!existsSync(resolve(COMMANDS_DIR, 'spawn-test-host'))) return 'spawn-test-host binary not built';
  return false;
}

// Minimal VFS for kernel
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

describe.skipIf(skipReason())('wasi-spawn: WasiChild host_process integration', { timeout: 30_000 }, () => {
  let kernel: Kernel;
  let vfs: SimpleVFS;

  beforeEach(async () => {
    vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
  });

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('spawn echo hello via host_process, capture stdout', async () => {
    const result = await kernel.exec('spawn-test-host echo');
    expect(result.stdout).toContain('stdout:hello');
    expect(result.stdout).toContain('exit:0');
    expect(result.stdout).toContain('PASS');
  });

  it('spawn failing command, verify non-zero exit code', async () => {
    const result = await kernel.exec('spawn-test-host fail');
    expect(result.stdout).toContain('exit:42');
    expect(result.stdout).toContain('PASS');
  });

  it('spawn with kill, verify signal termination', async () => {
    const result = await kernel.exec('spawn-test-host kill-test');
    expect(result.stdout).toContain('PASS');
  });

  it('spawn with custom env vars, verify captured', async () => {
    const result = await kernel.exec('spawn-test-host env-test');
    expect(result.stdout).toContain('PASS');
  });

  it('codex spawns echo and captures output', async () => {
    const result = await kernel.exec('codex echo hello');
    expect(result.stdout).toContain('hello');
  });
});
