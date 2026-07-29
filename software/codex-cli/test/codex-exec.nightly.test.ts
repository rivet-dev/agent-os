// Nightly: requires non-core registry commands.
/**
 * Integration tests for codex-exec command WASM binary.
 *
 * Verifies the codex-exec binary running in WasmVM can:
 *   - Print usage via --help
 *   - Print version via --version
 *   - Validate WASI stub crates via --stub-test
 *   - Accept a prompt argument and exit cleanly
 *   - Capture stdout/stderr correctly through the kernel
 *   - Be spawned from the shell (sh -c) via the kernel pipeline
 *   - Fail fast for session-turn mode until the real Codex agent is wired
 *
 * WASM binary tests are gated behind hasWasmBinaries.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { createWasmVmRuntime } from '@rivet-dev/agentos-test-harness';
import { COMMANDS_DIR, createKernel, describeIf, hasWasmBinaries } from '@rivet-dev/agentos-test-harness';
import type { Kernel } from '@rivet-dev/agentos-test-harness';

const hasCodexExec =
  process.env.AGENTOS_CODEX_WASM_E2E === '1' &&
  hasWasmBinaries &&
  existsSync(resolve(COMMANDS_DIR, 'codex-exec'));

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

// TODO(P6): requires codex-exec WASM artifact from an external fork build, intentionally excluded from the fast software-build gate.
describeIf(hasCodexExec, 'codex-exec command (WasmVM)', { timeout: 30_000 }, () => {
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
    // Prompt mode is currently a placeholder that accepts the prompt without echoing it.
    expect(result.stderr).toContain('headless prompt mode is not wired to the provider yet');
    expect(result.stderr).toContain('prompt received');
    expect(result.stderr).not.toContain('list all files');
  });

  it('accepts prompt from stdin without echoing it', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec', { stdin: 'stdin secret prompt\n' });
    expect(result.exitCode).toBe(0);
    expect(result.stderr).toContain('headless prompt mode is not wired to the provider yet');
    expect(result.stderr).toContain('prompt received');
    expect(result.stderr).not.toContain('stdin secret prompt');
  });

  it('rejects oversized stdin prompts', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec', { stdin: 'x'.repeat(64 * 1024 + 1) });
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain('stdin prompt exceeds');
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
    expect(result.stdout).toContain('USAGE');
  });

  it('captures stderr correctly through kernel.exec()', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec "test prompt"');
    // Headless mode outputs to stderr
    expect(result.stderr.length).toBeGreaterThan(0);
    expect(result.stderr).toContain('prompt received');
    expect(result.stderr).not.toContain('test prompt');
  });

  it('exits cleanly after completing a single prompt', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec "hello world"');
    // The process exits with code 0 (brush-shell wraps it)
    // Verify it doesn't hang — the exec() call resolves
    expect(result.stderr).toContain('prompt received');
    expect(result.stderr).not.toContain('hello world');
  });

  it('session-turn mode fails fast instead of calling a bespoke provider loop', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex-exec --session-turn');
    expect(result.stdout).toContain('"type":"error"');
    expect(result.stdout).toContain('real Codex agent package');
  });
});
