// Nightly: requires a non-core registry command.
/**
 * Integration tests for fd (fd-find) command.
 *
 * Verifies file finding with regex patterns, extension filters, type filters,
 * hidden file skipping, and empty directory handling via kernel.exec() with
 * real WASM binaries.
 *
 * Note: kernel.exec() wraps commands in sh -c. Brush-shell currently returns
 * exit code 17 for all child commands (benign "could not retrieve pid" issue).
 * Tests verify stdout correctness rather than exit code.
 */

import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { describe, it, expect, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agentos-test-harness';
import { COMMANDS_DIR, createKernel, describeIf, hasWasmBinaries, NodeFileSystem } from '@rivet-dev/agentos-test-harness';
import type { Kernel } from '@rivet-dev/agentos-test-harness';

let tempRoot: string | undefined;

/** Create a VFS pre-populated with a test directory structure */
async function createTestVFS(): Promise<NodeFileSystem> {
  tempRoot = await mkdtemp(join(tmpdir(), 'agentos-fd-'));

  // /project/
  //   src/
  //     main.js
  //     utils.js
  //     helpers.ts
  //   lib/
  //     parser.js
  //   docs/
  //     readme.md
  //   .hidden/
  //     secret.txt
  //   .gitignore
  //   config.json
  await writeFixture('/project/src/main.js', 'console.log("main")');
  await writeFixture('/project/src/utils.js', 'export {}');
  await writeFixture('/project/src/helpers.ts', 'export {}');
  await writeFixture('/project/lib/parser.js', 'module.exports = {}');
  await writeFixture('/project/docs/readme.md', '# Readme');
  await writeFixture('/project/.hidden/secret.txt', 'secret');
  await writeFixture('/project/.gitignore', 'node_modules');
  await writeFixture('/project/config.json', '{}');
  // /empty/ — empty directory
  await mkdir(join(tempRoot, 'empty'), { recursive: true });
  return new NodeFileSystem({ root: tempRoot });
}

async function writeFixture(path: string, contents: string): Promise<void> {
  if (!tempRoot) throw new Error('fixture root not initialized');
  const hostPath = join(tempRoot, path.replace(/^\/+/, ''));
  await mkdir(dirname(hostPath), { recursive: true });
  await writeFile(hostPath, contents);
}

/** Parse fd output lines, sorted for deterministic comparison */
function parseLines(stdout: string): string[] {
  return stdout.split('\n').filter(l => l.length > 0).sort();
}

describeIf(hasWasmBinaries, 'fd-find command', { timeout: 10_000 }, () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
    if (tempRoot) {
      await rm(tempRoot, { recursive: true, force: true });
      tempRoot = undefined;
    }
  });

  it('reports the upstream fd-find version', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd --version', {});
    expect(result.stdout.trim()).toBe('fd 10.4.2');
  });

  it('finds files matching regex pattern in current directory', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd main /project', {});
    const lines = parseLines(result.stdout);
    expect(lines).toContain('/project/src/main.js');
  });

  it('finds all .js files with -e js', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -e js . /project', {});
    const lines = parseLines(result.stdout);
    expect(lines).toContain('/project/src/main.js');
    expect(lines).toContain('/project/src/utils.js');
    expect(lines).toContain('/project/lib/parser.js');
    // .ts files should NOT match
    expect(lines).not.toContain('/project/src/helpers.ts');
  });

  it('finds only files with -t f', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -t f . /project', {});
    const lines = parseLines(result.stdout);
    // All entries should be files, not directories
    for (const line of lines) {
      const stat = await vfs.stat(line);
      expect(stat.isDirectory).toBe(false);
    }
    // Should include known files
    expect(lines).toContain('/project/src/main.js');
    expect(lines).toContain('/project/config.json');
  });

  it('finds only directories with -t d', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -t d . /project', {});
    const lines = parseLines(result.stdout);
    // All entries should be directories
    for (const line of lines) {
      const stat = await vfs.stat(line);
      expect(stat.isDirectory).toBe(true);
    }
    // Should include known directories (hidden skipped by default)
    expect(lines).toContain('/project/src/');
    expect(lines).toContain('/project/lib/');
    expect(lines).toContain('/project/docs/');
  });

  it('returns no results for empty directory', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd . /empty', {});
    expect(result.stdout.trim()).toBe('');
  });

  it('returns empty output when no files match pattern', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd zzzznonexistent /project', {});
    expect(result.stdout.trim()).toBe('');
  });

  it('skips hidden files and directories by default', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd . /project', {});
    const lines = parseLines(result.stdout);
    // Hidden files/dirs should NOT appear
    const hiddenEntries = lines.filter(l => {
      const parts = l.split('/');
      return parts.some(p => p.startsWith('.') && p.length > 1);
    });
    expect(hiddenEntries).toEqual([]);
  });

  it('includes hidden files with -H flag', async () => {
    const vfs = await createTestVFS();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec('fd -H . /project', {});
    const lines = parseLines(result.stdout);
    // Hidden items should now appear
    expect(lines).toContain('/project/.gitignore');
    expect(lines).toContain('/project/.hidden/');
  });
});
