/**
 * Integration tests for FD inheritance across process boundaries.
 *
 * Exercises shell redirections (> and <) which rely on FD table forking
 * and stdio overrides to work correctly. Each test creates a fresh kernel.
 *
 * Gracefully skipped when the WASM binary is not built.
 */

import { describe, it, expect, afterEach } from 'vitest';
import {
  createIntegrationKernel,
  skipUnlessWasmBuilt,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const skipReason = skipUnlessWasmBuilt();

describe.skipIf(skipReason)('FD inheritance', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('exec echo hello > /tmp/out.txt writes to VFS file', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    const result = await ctx.kernel.exec('echo hello > /tmp/out.txt');
    expect(result.exitCode).toBe(0);

    const content = await ctx.vfs.readTextFile('/tmp/out.txt');
    expect(content.trim()).toBe('hello');
  });

  it('exec cat < /tmp/in.txt reads from VFS file', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    await ctx.vfs.writeFile('/tmp/in.txt', 'input data\n');

    const result = await ctx.kernel.exec('cat < /tmp/in.txt');
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('input data');
  });

  it('exec with append >> accumulates output', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    await ctx.kernel.exec('echo first > /tmp/append.txt');
    await ctx.kernel.exec('echo second >> /tmp/append.txt');

    const content = await ctx.vfs.readTextFile('/tmp/append.txt');
    expect(content).toContain('first');
    expect(content).toContain('second');
  });

  it('piped output preserves data across redirection', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    await ctx.vfs.writeFile('/tmp/nums.txt', 'a\nb\nc\n');

    const result = await ctx.kernel.exec('cat /tmp/nums.txt | wc -l');
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('3');
  });

  it('exec with combined stdin redirect and stdout redirect', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    await ctx.vfs.writeFile('/tmp/data.txt', 'vfs-content\n');

    // Read from file via < and write to file via >. Full FD inheritance chain.
    const result = await ctx.kernel.exec(
      'cat < /tmp/data.txt > /tmp/out.txt',
    );
    expect(result.exitCode).toBe(0);

    const content = await ctx.vfs.readTextFile('/tmp/out.txt');
    expect(content.trim()).toBe('vfs-content');
  });
});
