/**
 * Integration tests for WasmVM dynamic module loading pipeline.
 *
 * Verifies the full pipeline: kernel -> CommandRegistry -> WasmVmRuntimeDriver
 * -> ModuleCache -> Worker. Tests cover commandDirs discovery, symlink alias
 * resolution, on-demand discovery through the kernel, path-based resolution,
 * and backwards compatibility.
 *
 * Tests requiring real WASM execution register only when binaries are available.
 */

import { describe, it, expect, afterEach, vi } from 'vitest';
import { createWasmVmRuntime, WASMVM_COMMANDS } from '@rivet-dev/agent-os-core/test/runtime';
import type { WasmVmRuntimeOptions } from '@rivet-dev/agent-os-core/test/runtime';
import { COMMANDS_DIR, createKernel, describeIf, hasWasmBinaries } from '../helpers.js';
import type {
  DriverProcess,
  Kernel,
  KernelInterface,
  KernelRuntimeDriver as RuntimeDriver,
  ProcessContext,
} from '../helpers.js';
import { writeFile, mkdir, rm, symlink } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

// Valid WASM magic: \0asm + version 1
const VALID_WASM = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

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
  async mkdir(path: string, _options?: { recursive?: boolean }) { this.dirs.add(path); }
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
  async removeFile(path: string) { this.files.delete(path); }
  async removeDir(path: string) { this.dirs.delete(path); }
  async rename(oldPath: string, newPath: string) {
    const data = this.files.get(oldPath);
    if (data) { this.files.set(newPath, data); this.files.delete(oldPath); }
  }
  async realpath(path: string) { return path; }
  async symlink(_target: string, _linkPath: string) {}
  async readlink(_path: string): Promise<string> { return ''; }
  async lstat(path: string) { return this.stat(path); }
  async link(_old: string, _new: string) {}
  async chmod(_path: string, _mode: number) {}
  async chown(_path: string, _uid: number, _gid: number) {}
  async utimes(_path: string, _atime: number, _mtime: number) {}
  async truncate(_path: string, _length: number) {}
}

/** Create a temp dir with WASM command binaries for testing. */
async function createCommandDir(commands: string[]): Promise<string> {
  const dir = join(tmpdir(), `wasmvm-integ-${Date.now()}-${Math.random().toString(36).slice(2)}`);
  await mkdir(dir, { recursive: true });
  for (const cmd of commands) {
    await writeFile(join(dir, cmd), VALID_WASM);
  }
  return dir;
}

// -------------------------------------------------------------------------
// Integration Tests
// -------------------------------------------------------------------------

describe('Dynamic module loading — integration', () => {
  const tempDirs: string[] = [];

  afterEach(async () => {
    for (const dir of tempDirs) {
      await rm(dir, { recursive: true, force: true }).catch(() => {});
    }
    tempDirs.length = 0;
  });

  /** Helper to create + track a temp dir. */
  async function makeTempDir(commands: string[]): Promise<string> {
    const dir = await createCommandDir(commands);
    tempDirs.push(dir);
    return dir;
  }

  describe('commandDirs discovery through kernel', () => {
    it('kernel registers commands discovered from commandDirs at init', async () => {
      const dir = await makeTempDir(['ls', 'cat', 'grep']);
      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      expect(kernel.commands.get('ls')).toBe('wasmvm');
      expect(kernel.commands.get('cat')).toBe('wasmvm');
      expect(kernel.commands.get('grep')).toBe('wasmvm');
      expect(kernel.commands.size).toBe(3);

      await kernel.dispose();
    });

    it('first dir in commandDirs wins on naming conflict', async () => {
      const dir1 = await makeTempDir(['ls', 'cat']);
      const dir2 = await makeTempDir(['ls', 'grep']);

      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir1, dir2] }) as any;
      await kernel.mount(driver);

      // All 3 unique commands registered
      expect(kernel.commands.get('ls')).toBe('wasmvm');
      expect(kernel.commands.get('cat')).toBe('wasmvm');
      expect(kernel.commands.get('grep')).toBe('wasmvm');
      // ls resolved from dir1 (first match wins)
      expect(driver._commandPaths.get('ls')).toBe(join(dir1, 'ls'));

      await kernel.dispose();
    });

    it('non-WASM files in commandDirs are ignored by kernel', async () => {
      const dir = await makeTempDir(['ls']);
      await writeFile(join(dir, 'README.md'), 'documentation');
      await writeFile(join(dir, 'config.json'), '{}');

      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      expect(kernel.commands.size).toBe(1);
      expect(kernel.commands.get('ls')).toBe('wasmvm');
      expect(kernel.commands.has('README.md')).toBe(false);
      expect(kernel.commands.has('config.json')).toBe(false);

      await kernel.dispose();
    });
  });

  describe('symlink alias resolution', () => {
    it('symlink aliases are followed during commandDirs scan', async () => {
      const dir = await makeTempDir(['grep']);
      // Create symlink: egrep -> grep
      await symlink(join(dir, 'grep'), join(dir, 'egrep'));
      // Create symlink: fgrep -> grep
      await symlink(join(dir, 'grep'), join(dir, 'fgrep'));

      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      const mockKernel: Partial<KernelInterface> = {};
      await driver.init(mockKernel as KernelInterface);

      // All three should be discovered (symlinks are regular files to readdir)
      expect(driver.commands).toContain('grep');
      expect(driver.commands).toContain('egrep');
      expect(driver.commands).toContain('fgrep');
    });

    it('symlink aliases resolve to valid binary paths', async () => {
      const dir = await makeTempDir(['grep']);
      await symlink(join(dir, 'grep'), join(dir, 'egrep'));

      const driver = createWasmVmRuntime({ commandDirs: [dir] }) as any;
      const mockKernel: Partial<KernelInterface> = {};
      await driver.init(mockKernel as KernelInterface);

      // Both point to valid paths
      expect(driver._commandPaths.get('grep')).toBe(join(dir, 'grep'));
      expect(driver._commandPaths.get('egrep')).toBe(join(dir, 'egrep'));
    });

    it('symlink aliases are registered in kernel', async () => {
      const dir = await makeTempDir(['grep']);
      await symlink(join(dir, 'grep'), join(dir, 'egrep'));
      await symlink(join(dir, 'grep'), join(dir, 'fgrep'));

      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      expect(kernel.commands.get('grep')).toBe('wasmvm');
      expect(kernel.commands.get('egrep')).toBe('wasmvm');
      expect(kernel.commands.get('fgrep')).toBe('wasmvm');

      await kernel.dispose();
    });

    it('symlinks to non-WASM targets are ignored', async () => {
      const dir = await makeTempDir([]);
      await writeFile(join(dir, 'readme'), 'not wasm');
      await symlink(join(dir, 'readme'), join(dir, 'bad-link'));

      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      const mockKernel: Partial<KernelInterface> = {};
      await driver.init(mockKernel as KernelInterface);

      expect(driver.commands).not.toContain('readme');
      expect(driver.commands).not.toContain('bad-link');
      expect(driver.commands).toEqual([]);
    });
  });

  describe('on-demand discovery through kernel', () => {
    it('kernel discovers a binary added after init via tryResolve', async () => {
      const dir = await makeTempDir(['ls']);
      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      // Only ls is known at mount time
      expect(kernel.commands.has('new-cmd')).toBe(false);

      // Drop a new binary after init
      await writeFile(join(dir, 'new-cmd'), VALID_WASM);

      // Spawning new-cmd triggers tryResolve in kernel → driver discovers it
      // Since it's a fake WASM, spawn will fail, but the command should be registered
      try {
        kernel.spawn('new-cmd', []);
      } catch {
        // Expected: spawn will throw because the binary is just magic bytes
      }

      // After tryResolve succeeded, the command is registered
      expect(kernel.commands.get('new-cmd')).toBe('wasmvm');

      await kernel.dispose();
    });

    it('after tryResolve succeeds, subsequent spawns resolve without calling tryResolve again', async () => {
      const dir = await makeTempDir(['ls']);
      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      // Add a new binary
      await writeFile(join(dir, 'extra'), VALID_WASM);

      // First spawn triggers tryResolve
      try { kernel.spawn('extra', []); } catch {}
      expect(kernel.commands.get('extra')).toBe('wasmvm');

      // Spy on tryResolve — second spawn should NOT call it (registry hit)
      const tryResolveSpy = vi.spyOn(driver as any, 'tryResolve');
      try { kernel.spawn('extra', []); } catch {}
      expect(tryResolveSpy).not.toHaveBeenCalled();

      tryResolveSpy.mockRestore();
      await kernel.dispose();
    });

    it('tryResolve normalizes path-based commands to basename before lookup', async () => {
      const dir = await makeTempDir(['alpha']);
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      const mockKernel: Partial<KernelInterface> = {};
      await driver.init(mockKernel as KernelInterface);

      // Path-based tryResolve should normalize /bin/alpha → alpha
      expect(driver.tryResolve('/bin/alpha')).toBe(true);
      expect(driver.tryResolve('/usr/local/bin/alpha')).toBe(true);
      // Bare name still works
      expect(driver.tryResolve('alpha')).toBe(true);
      // Nonexistent commands still return false
      expect(driver.tryResolve('/bin/nonexistent')).toBe(false);
    });

    it('tryResolve returning false for all drivers results in ENOENT', async () => {
      const dir = await makeTempDir(['ls']);
      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      expect(() => kernel.spawn('nonexistent', [])).toThrow(/ENOENT/);
      await kernel.dispose();
    });
  });

  describe('path-based resolution through kernel', () => {
    it('/bin/ls resolves to ls through kernel', async () => {
      const dir = await makeTempDir(['ls', 'cat']);
      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      // Path-based resolution: /bin/ls -> ls
      // Spawn won't execute (fake WASM) but shouldn't throw ENOENT
      let threw = false;
      try {
        kernel.spawn('/bin/ls', []);
      } catch (e: any) {
        if (e.message?.includes('ENOENT')) threw = true;
      }
      expect(threw).toBe(false);

      await kernel.dispose();
    });

    it('/usr/bin/grep resolves to grep through kernel', async () => {
      const dir = await makeTempDir(['grep']);
      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      await kernel.mount(driver);

      let threw = false;
      try {
        kernel.spawn('/usr/bin/grep', ['hello']);
      } catch (e: any) {
        if (e.message?.includes('ENOENT')) threw = true;
      }
      expect(threw).toBe(false);

      await kernel.dispose();
    });
  });

  describe('WASMVM_COMMANDS export', () => {
    it('WASMVM_COMMANDS is a frozen static list with 90+ commands', () => {
      expect(WASMVM_COMMANDS.length).toBeGreaterThanOrEqual(90);
      expect(Object.isFrozen(WASMVM_COMMANDS)).toBe(true);
      expect(WASMVM_COMMANDS).toContain('sh');
      expect(WASMVM_COMMANDS).toContain('ls');
      expect(WASMVM_COMMANDS).toContain('grep');
    });

    it('commandDirs mode: driver.commands reflects filesystem scan, not WASMVM_COMMANDS', async () => {
      const dir = await makeTempDir(['alpha', 'beta', 'gamma']);
      const driver = createWasmVmRuntime({ commandDirs: [dir] });
      const mockKernel: Partial<KernelInterface> = {};
      await driver.init(mockKernel as KernelInterface);

      // Commands reflect what's on disk, not the static WASMVM_COMMANDS list
      expect(driver.commands).toEqual(expect.arrayContaining(['alpha', 'beta', 'gamma']));
      expect(driver.commands.length).toBe(3);
      expect(driver.commands).not.toContain('sh');
    });

    it('legacy mode: driver.commands matches WASMVM_COMMANDS', () => {
      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
      const driver = createWasmVmRuntime({ wasmBinaryPath: '/fake' });
      expect(driver.commands.length).toBe(WASMVM_COMMANDS.length);
      for (const cmd of WASMVM_COMMANDS) {
        expect(driver.commands).toContain(cmd);
      }
      warnSpy.mockRestore();
    });
  });

  describe('backwards compatibility — deprecation', () => {
    it('wasmBinaryPath mode mounts to kernel and registers all commands', async () => {
      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
      const vfs = new SimpleVFS();
      const kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ wasmBinaryPath: '/fake' });
      await kernel.mount(driver);

      expect(kernel.commands.get('sh')).toBe('wasmvm');
      expect(kernel.commands.get('ls')).toBe('wasmvm');
      expect(kernel.commands.size).toBeGreaterThanOrEqual(90);
      expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('deprecated'));

      warnSpy.mockRestore();
      await kernel.dispose();
    });
  });

  describeIf(hasWasmBinaries, 'module cache integration', () => {
    let kernel: Kernel;

    afterEach(async () => {
      await kernel?.dispose();
    });

    it('module cache returns same WebAssembly.Module for repeated resolves', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }) as any;
      await kernel.mount(driver);

      // Cache starts empty
      expect(driver._moduleCache.size).toBe(0);

      // First exec compiles + caches
      const r1 = await kernel.exec('echo first');
      expect(r1.exitCode).toBe(0);
      expect(driver._moduleCache.size).toBe(1);

      // Second exec reuses cached module (same command = same cache entry)
      const r2 = await kernel.exec('echo second');
      expect(r2.exitCode).toBe(0);
      expect(driver._moduleCache.size).toBe(1);
    }, 30_000);

    it('different commands get separate cache entries', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      const driver = createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }) as any;
      await kernel.mount(driver);

      // Each standalone binary gets its own cache entry
      await kernel.exec('echo hello');
      await kernel.exec('true');
      expect(driver._moduleCache.size).toBe(2);
    }, 30_000);
  });
});
