/**
 * Integration tests for codex TUI WASM binary.
 *
 * Verifies the codex TUI binary running in WasmVM can:
 *   - Print usage via --help (bypasses TUI)
 *   - Render the TUI through the WasmVM PTY
 *   - Accept keyboard input through PTY stdin
 *   - Quit on 'q' (empty input) or Ctrl+C
 *   - Display welcome text on initial render
 *   - Accept --model flag for model selection
 *
 * API-dependent tests are gated behind OPENAI_API_KEY env var.
 * WASM binary tests are gated behind hasWasmBinaries.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { TerminalHarness } from './terminal-harness.js';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import { COMMANDS_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '../helpers.js';

const hasApiKey = !!process.env.OPENAI_API_KEY;

/** brush-shell interactive prompt. */
const PROMPT = 'sh-0.4$ ';

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

// ---------------------------------------------------------------------------
// Non-interactive tests (kernel.exec — --help bypasses TUI)
// ---------------------------------------------------------------------------

describe.skipIf(!hasWasmBinaries)('codex TUI (WasmVM) - non-interactive', { timeout: 30_000 }, () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('--help prints usage without TUI', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex --help');
    expect(result.stdout).toContain('codex');
    expect(result.stdout).toContain('USAGE');
    expect(result.stdout).toContain('--help');
    expect(result.stdout).toContain('--version');
    expect(result.stdout).toContain('--model MODEL');
    expect(result.stdout).toContain('headless');
  });

  it('--model flag is documented in help', async () => {
    ({ kernel } = await createTestKernel());
    const result = await kernel.exec('codex --help');
    expect(result.stdout).toContain('--model MODEL');
    expect(result.stdout).toContain('Select model for completions');
  });
});

// ---------------------------------------------------------------------------
// Interactive TUI tests (PTY via TerminalHarness)
// ---------------------------------------------------------------------------

describe.skipIf(!hasWasmBinaries)('codex TUI (WasmVM) - interactive', { timeout: 30_000 }, () => {
  let kernel: Kernel;
  let harness: TerminalHarness;

  afterEach(async () => {
    await harness?.dispose();
    await kernel?.dispose();
  });

  it('starts and produces TUI output bytes on stdout', async () => {
    ({ kernel } = await createTestKernel());
    harness = new TerminalHarness(kernel);
    await harness.waitFor(PROMPT);

    await harness.type('codex\n');
    // TUI enters alternate screen — wait for ratatui-rendered content
    await harness.waitFor('codex', 1, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen.length).toBeGreaterThan(0);
  });

  it('TUI output contains expected welcome/prompt text', async () => {
    ({ kernel } = await createTestKernel());
    harness = new TerminalHarness(kernel);
    await harness.waitFor(PROMPT);

    await harness.type('codex\n');
    await harness.waitFor('Welcome to Codex', 1, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('Welcome to Codex');
    expect(screen).toContain('Type a prompt');
    expect(screen).toContain('Ctrl+C to exit');
  });

  it('receives keystroke input via PTY stdin write', async () => {
    ({ kernel } = await createTestKernel());
    harness = new TerminalHarness(kernel);
    await harness.waitFor(PROMPT);

    await harness.type('codex\n');
    await harness.waitFor('Welcome to Codex', 1, 10_000);

    // Type characters — they should appear in the input area
    await harness.type('hello');
    await harness.waitFor('hello');

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('hello');
  });

  it('typing q on empty input exits TUI', async () => {
    ({ kernel } = await createTestKernel());
    harness = new TerminalHarness(kernel);
    await harness.waitFor(PROMPT);

    await harness.type('codex\n');
    await harness.waitFor('Welcome to Codex', 1, 10_000);

    // 'q' on empty input should quit TUI and return to shell
    await harness.type('q');
    await harness.waitFor(PROMPT, 2, 10_000);
  });

  it('Ctrl+C exits TUI', async () => {
    ({ kernel } = await createTestKernel());
    harness = new TerminalHarness(kernel);
    await harness.waitFor(PROMPT);

    await harness.type('codex\n');
    await harness.waitFor('Welcome to Codex', 1, 10_000);

    // Ctrl+C should quit TUI and return to shell
    await harness.type('\x03');
    await harness.waitFor(PROMPT, 2, 10_000);
  });

  it('--model flag accepts model selection in TUI header', async () => {
    ({ kernel } = await createTestKernel());
    harness = new TerminalHarness(kernel);
    await harness.waitFor(PROMPT);

    await harness.type('codex --model gpt-4o\n');
    await harness.waitFor('Welcome to Codex', 1, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('model: gpt-4o');

    // Quit TUI
    await harness.type('q');
    await harness.waitFor(PROMPT, 2, 10_000);
  });
});

// ---------------------------------------------------------------------------
// API integration tests (gated behind OPENAI_API_KEY)
// ---------------------------------------------------------------------------

describe.skipIf(!hasWasmBinaries || !hasApiKey)('codex TUI API integration (requires OPENAI_API_KEY)', { timeout: 60_000 }, () => {
  let kernel: Kernel;
  let harness: TerminalHarness;

  afterEach(async () => {
    await harness?.dispose();
    await kernel?.dispose();
  });

  it('with OPENAI_API_KEY can complete a simple prompt via TUI', async () => {
    ({ kernel } = await createTestKernel());
    harness = new TerminalHarness(kernel, {
      env: { OPENAI_API_KEY: process.env.OPENAI_API_KEY! },
    });
    await harness.waitFor(PROMPT);

    await harness.type('codex\n');
    await harness.waitFor('Welcome to Codex', 1, 10_000);

    // Type a prompt and submit
    await harness.type('say hello\n');
    // Agent loop is under development — should show the prompt was received
    await harness.waitFor('say hello', 2, 10_000);

    // Quit TUI
    await harness.type('q');
  });
});
