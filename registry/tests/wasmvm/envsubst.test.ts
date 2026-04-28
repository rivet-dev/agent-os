/**
 * Integration tests for envsubst C command.
 *
 * Verifies environment variable substitution in stdin: $VAR, ${VAR},
 * ${VAR:-default}, and escaped \$VAR via kernel.exec() with real WASM binaries.
 *
 * Note: kernel.exec() wraps commands in sh -c. Brush-shell currently returns
 * exit code 17 for all child commands (benign "could not retrieve pid" issue).
 * Tests verify stdout correctness rather than exit code.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import { C_BUILD_DIR, COMMANDS_DIR, createKernel } from '../helpers.js';
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

describe('envsubst command', () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('substitutes $VAR with environment variable value', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
    );

    const result = await kernel.exec('envsubst', {
      stdin: 'Hello $USER\n',
      env: { USER: 'world' },
    });
    expect(result.stdout.trim()).toBe('Hello world');
  });

  it('substitutes ${VAR:-default} with fallback for undefined var', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
    );

    const result = await kernel.exec('envsubst', {
      stdin: '${UNDEFINED:-fallback}\n',
      env: {},
    });
    expect(result.stdout.trim()).toBe('fallback');
  });

  it('passes through escaped \\$VAR literally', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
    );

    const result = await kernel.exec('envsubst', {
      stdin: '\\$ESCAPED\n',
      env: { ESCAPED: 'nope' },
    });
    expect(result.stdout.trim()).toBe('$ESCAPED');
  });

  it('substitutes multiple variables in one line', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
    );

    const result = await kernel.exec('envsubst', {
      stdin: '$GREETING $NAME from $PLACE\n',
      env: { GREETING: 'Hello', NAME: 'Alice', PLACE: 'Wonderland' },
    });
    expect(result.stdout.trim()).toBe('Hello Alice from Wonderland');
  });

  it('replaces undefined variables with empty string', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
    );

    const result = await kernel.exec('envsubst', {
      stdin: 'before${MISSING}after\n',
      env: {},
    });
    expect(result.stdout.trim()).toBe('beforeafter');
  });

  it('handles ${VAR} brace syntax with defined variable', async () => {
    const vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
    );

    const result = await kernel.exec('envsubst', {
      stdin: '${APP_NAME}_config\n',
      env: { APP_NAME: 'myapp' },
    });
    expect(result.stdout.trim()).toBe('myapp_config');
  });
});
