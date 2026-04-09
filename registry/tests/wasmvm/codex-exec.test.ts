/**
 * Integration tests for codex-exec headless agent WASM binary.
 *
 * Verifies the codex-exec binary running in WasmVM can:
 *   - Print usage via --help
 *   - Print version via --version
 *   - Validate WASI stub crates via --stub-test
 *   - Accept a prompt argument and exit cleanly
 *   - Capture stdout/stderr correctly through the kernel
 *   - Be spawned from the shell (sh -c) via the kernel pipeline
 *
 * API-dependent tests are gated behind OPENAI_API_KEY env var.
 * WASM binary tests are gated behind hasWasmBinaries.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import { COMMANDS_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '../helpers.js';

const hasApiKey = !!process.env.OPENAI_API_KEY;

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

async function createTestKernel(): Promise<{ kernel: Kernel; vfs: SimpleVFS }> {
  const vfs = new SimpleVFS();
  const kernel = createKernel({ filesystem: vfs as any });
  await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
  return { kernel, vfs };
}

describe.skipIf(!hasWasmBinaries)('codex-exec headless agent (WasmVM)', { timeout: 30_000 }, () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('--help prints usage without errors', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec --help');
    expect(result.stdout).toContain('codex-exec');
    expect(result.stdout).toContain('USAGE');
    expect(result.stdout).toContain('headless');
    expect(result.stdout).toContain('--help');
    expect(result.stdout).toContain('--version');
  });

  it('--version prints version', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec --version');
    expect(result.stdout).toMatch(/codex-exec \d+\.\d+\.\d+/);
  });

  it('--stub-test validates WASI stub crates', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec --stub-test');
    expect(result.stdout).toContain('network-proxy');
    expect(result.stdout).toContain('otel');
    expect(result.stdout).toContain('stub-test: all stubs validated successfully');
  });

  it('accepts prompt as argument and exits cleanly', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec "list all files"');
    // Headless mode is under development — prints prompt to stderr and exits
    expect(result.stderr).toContain('headless agent mode is under development');
    expect(result.stderr).toContain('list all files');
  });

  it('prints error when no prompt is provided via arg', async () => {
    ({ kernel } = await createTestKernel());
    // codex-exec with no args reads stdin; since stdin is empty pipe it gets empty prompt
    const result = await kernel.exec('codex-exec');
    // Should get an error about no prompt or the stdin read returns empty
    expect(result.stderr).toContain('codex-exec');
  });

  it('can be spawned from shell via sh -c pipeline', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('sh -c "codex-exec --help"');
    expect(result.stdout).toContain('codex-exec');
    expect(result.stdout).toContain('USAGE');
  });

  it('captures stdout correctly through kernel.exec()', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec --help');
    // Verify stdout is non-empty and contains expected structured output
    expect(result.stdout.length).toBeGreaterThan(0);
    expect(result.stdout).toContain('OPTIONS');
    expect(result.stdout).toContain('DESCRIPTION');
  });

  it('captures stderr correctly through kernel.exec()', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec "test prompt"');
    // Headless mode outputs to stderr
    expect(result.stderr.length).toBeGreaterThan(0);
    expect(result.stderr).toContain('prompt: test prompt');
  });

  it('exits cleanly after completing a single prompt', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec "hello world"');
    // The process exits with code 0 (brush-shell wraps it)
    // Verify it doesn't hang — the exec() call resolves
    expect(result.stderr).toContain('hello world');
  });
});

describe.skipIf(!hasWasmBinaries || !hasApiKey)('codex-exec API integration (requires OPENAI_API_KEY)', { timeout: 60_000 }, () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('with OPENAI_API_KEY env var produces output', async () => {
    const vfs = new SimpleVFS();
    const kernel_local = createKernel({ filesystem: vfs as any });
    kernel = kernel_local;
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    // Since the agent loop is a placeholder, this test verifies that
    // the binary accepts the prompt and exits without crashing when
    // the API key is in the environment. Full API integration will be
    // tested when codex-core is wired in.
    const result = await kernel.exec('codex-exec "say hello"');
    // Should at minimum print the prompt back and exit
    expect(result.stderr).toContain('say hello');
  });
});
