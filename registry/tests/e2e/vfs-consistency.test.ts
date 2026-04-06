/**
 * Cross-runtime VFS consistency tests.
 *
 * Verifies that file writes in one runtime are immediately visible to
 * reads in another runtime, since all runtimes share the kernel VFS.
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

describe.skipIf(skipReason)('cross-runtime VFS consistency', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('kernel write visible to Node', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    await ctx.kernel.writeFile('/tmp/test.txt', 'hello');

    const result = await ctx.kernel.exec(
      `node -e "process.stdout.write(require('fs').readFileSync('/tmp/test.txt','utf8'))"`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('hello');
  });

  it('Node write visible to WasmVM', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // Node writes a file
    const writeResult = await ctx.kernel.exec(
      `node -e "require('fs').writeFileSync('/tmp/node-wrote.txt','from-node')"`,
    );
    expect(writeResult.exitCode).toBe(0);

    // WasmVM reads it via cat
    const readResult = await ctx.kernel.exec('cat /tmp/node-wrote.txt');
    expect(readResult.exitCode).toBe(0);
    expect(readResult.stdout).toContain('from-node');
  });

  it('Node write visible to kernel API', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const writeResult = await ctx.kernel.exec(
      `node -e "require('fs').writeFileSync('/tmp/k.txt','data')"`,
    );
    expect(writeResult.exitCode).toBe(0);

    const content = await ctx.vfs.readTextFile('/tmp/k.txt');
    expect(content).toBe('data');
  });

  it('directory listing consistent across runtimes', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // Create 3 files via kernel API
    await ctx.kernel.writeFile('/tmp/a.txt', 'a');
    await ctx.kernel.writeFile('/tmp/b.txt', 'b');
    await ctx.kernel.writeFile('/tmp/c.txt', 'c');

    // WasmVM ls
    const lsResult = await ctx.kernel.exec('ls /tmp');
    expect(lsResult.exitCode).toBe(0);

    // Node readdirSync
    const nodeResult = await ctx.kernel.exec(
      `node -e "console.log(require('fs').readdirSync('/tmp').sort().join(','))"`,
    );
    expect(nodeResult.exitCode).toBe(0);

    // Both should list the same files
    const lsFiles = lsResult.stdout
      .trim()
      .split(/\s+/)
      .filter(Boolean)
      .sort();
    const nodeFiles = nodeResult.stdout.trim().split(',').filter(Boolean).sort();

    expect(lsFiles).toContain('a.txt');
    expect(lsFiles).toContain('b.txt');
    expect(lsFiles).toContain('c.txt');
    expect(nodeFiles).toContain('a.txt');
    expect(nodeFiles).toContain('b.txt');
    expect(nodeFiles).toContain('c.txt');
  });

  it('ENOENT consistent across runtimes', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // WasmVM cat nonexistent file
    const catResult = await ctx.kernel.exec('cat /nonexistent');
    expect(catResult.exitCode).not.toBe(0);

    // Node readFileSync nonexistent file
    const nodeResult = await ctx.kernel.exec(
      `node -e "require('fs').readFileSync('/nonexistent')"`,
    );
    expect(nodeResult.exitCode).not.toBe(0);
  });
});
