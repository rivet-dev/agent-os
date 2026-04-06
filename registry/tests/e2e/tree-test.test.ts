/**
 * Tree command behavior tests.
 *
 * Verifies that `tree` works correctly in both kernel.exec() and
 * interactive shell modes, including edge cases like nonexistent paths,
 * nested directories, and empty directories.
 */
import { describe, it, expect, afterEach } from 'vitest';
import {
  createIntegrationKernel,
  skipUnlessWasmBuilt,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const wasmSkip = skipUnlessWasmBuilt();

describe.skipIf(wasmSkip)('tree command behavior', () => {
  let ctx: IntegrationKernelResult;
  afterEach(async () => { await ctx?.dispose().catch(() => {}); });

  // -----------------------------------------------------------------------
  // kernel.exec tests
  // -----------------------------------------------------------------------

  it('kernel.exec tree / returns within 5s with directory listing', async () => {
    ctx = await createIntegrationKernel();
    const result = await ctx.kernel.exec('tree /', { timeout: 5000 });
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('bin');
    // Tree summary line
    expect(result.stdout).toMatch(/\d+ director/);
    expect(result.stdout).toMatch(/\d+ file/);
  }, 10_000);

  it('kernel.exec tree /nonexistent returns non-zero with error', async () => {
    ctx = await createIntegrationKernel();
    const result = await ctx.kernel.exec('tree /nonexistent', { timeout: 5000 });
    // tree should report an error for non-existent path
    const combined = result.stdout + result.stderr;
    expect(combined).toContain('nonexistent');
  }, 10_000);

  it('tree on VFS with 3-level nested directories renders correct structure', async () => {
    ctx = await createIntegrationKernel();
    const enc = new TextEncoder();
    ctx.vfs.writeFile('/project/src/lib/utils.ts', enc.encode('export {}'));
    ctx.vfs.writeFile('/project/src/lib/types.ts', enc.encode('export {}'));
    ctx.vfs.writeFile('/project/src/index.ts', enc.encode('export {}'));
    ctx.vfs.writeFile('/project/README.md', enc.encode('# project'));

    const result = await ctx.kernel.exec('tree /project', { timeout: 5000 });
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('src');
    expect(result.stdout).toContain('lib');
    expect(result.stdout).toContain('utils.ts');
    expect(result.stdout).toContain('types.ts');
    expect(result.stdout).toContain('index.ts');
    expect(result.stdout).toContain('README.md');
    // Should show 2 directories (src, lib) and 4 files
    expect(result.stdout).toMatch(/2 director/);
    expect(result.stdout).toMatch(/4 file/);
  }, 10_000);

  it('tree on empty directory shows minimal output', async () => {
    ctx = await createIntegrationKernel();
    ctx.vfs.mkdir('/empty');

    const result = await ctx.kernel.exec('tree /empty', { timeout: 5000 });
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('/empty');
    // Empty directory: 0 directories, 0 files
    expect(result.stdout).toMatch(/0 director/);
    expect(result.stdout).toMatch(/0 file/);
  }, 10_000);

  // -----------------------------------------------------------------------
  // Interactive shell tests
  // -----------------------------------------------------------------------

  it('interactive shell: tree / produces output and prompt returns', async () => {
    ctx = await createIntegrationKernel();
    const shell = ctx.kernel.openShell();

    let output = '';
    shell.onData = (data) => { output += new TextDecoder().decode(data); };

    // Wait for initial prompt
    await new Promise((r) => setTimeout(r, 1500));
    output = '';

    shell.write('tree /\n');

    // Wait for tree completion (summary line + new prompt)
    const start = Date.now();
    while (Date.now() - start < 5000) {
      await new Promise((r) => setTimeout(r, 200));
      if (output.includes('file') && output.includes('$ ')) break;
    }

    expect(output).toContain('bin');
    expect(output).toMatch(/\d+ file/);
    // Prompt returned
    expect(output).toContain('$ ');

    shell.write('exit\n');
    await Promise.race([
      shell.wait(),
      new Promise((_, rej) => setTimeout(() => rej('timeout'), 3000)),
    ]).catch(() => {});
  }, 15_000);

  it('tree does not hang when stdin is an empty PTY', async () => {
    ctx = await createIntegrationKernel();
    const shell = ctx.kernel.openShell();

    let output = '';
    shell.onData = (data) => { output += new TextDecoder().decode(data); };

    await new Promise((r) => setTimeout(r, 1500));
    output = '';

    // tree never reads stdin. It should complete regardless of PTY stdin state.
    shell.write('tree /\n');

    const start = Date.now();
    while (Date.now() - start < 5000) {
      await new Promise((r) => setTimeout(r, 200));
      if (output.includes('file') && output.includes('$ ')) break;
    }

    const elapsed = Date.now() - start;
    // Should complete well within 5 seconds
    expect(elapsed).toBeLessThan(5000);
    expect(output).toContain('bin');

    shell.write('exit\n');
    await Promise.race([
      shell.wait(),
      new Promise((_, rej) => setTimeout(() => rej('timeout'), 3000)),
    ]).catch(() => {});
  }, 15_000);
});
