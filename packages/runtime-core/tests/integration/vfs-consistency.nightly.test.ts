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
  describeIf,
  createIntegrationKernel,
  skipUnlessWasmBuilt,
} from '@rivet-dev/agentos-vm-test-harness';
import type { IntegrationKernelResult } from '@rivet-dev/agentos-vm-test-harness';

const skipReason = skipUnlessWasmBuilt();

describeIf(!skipReason, 'cross-runtime VFS consistency', { timeout: 30_000 }, () => {
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

  // Guest deletions must propagate into the kernel VFS instead of being
  // resurrected by the (otherwise additive) shadow->kernel sync. This is the
  // failure mode behind git's failed-clone junk surviving `remove_junk`.
  it('guest rm -r removes the tree from the kernel VFS', async () => {
    ctx = await createIntegrationKernel();

    const create = await ctx.kernel.exec(
      'sh -c "mkdir -p /tmp/doomed/nested && echo payload > /tmp/doomed/nested/file.txt"',
    );
    expect(create.exitCode).toBe(0);
    expect(await ctx.vfs.exists('/tmp/doomed/nested/file.txt')).toBe(true);

    const remove = await ctx.kernel.exec('rm -r /tmp/doomed');
    expect(remove.exitCode).toBe(0);

    expect(await ctx.vfs.exists('/tmp/doomed/nested/file.txt')).toBe(false);
    expect(await ctx.vfs.exists('/tmp/doomed/nested')).toBe(false);
    expect(await ctx.vfs.exists('/tmp/doomed')).toBe(false);

    const ls = await ctx.kernel.exec('ls /tmp/doomed');
    expect(ls.exitCode).not.toBe(0);
  });

  it('guest rmdir on an empty directory succeeds and propagates', async () => {
    ctx = await createIntegrationKernel();

    const mkdir = await ctx.kernel.exec('mkdir /tmp/empty-dir');
    expect(mkdir.exitCode).toBe(0);
    expect(await ctx.vfs.exists('/tmp/empty-dir')).toBe(true);

    const rmdir = await ctx.kernel.exec('rmdir /tmp/empty-dir');
    expect(rmdir.exitCode, rmdir.stderr).toBe(0);
    expect(await ctx.vfs.exists('/tmp/empty-dir')).toBe(false);
  });

  it('guest unlink removes a dangling symlink so its directory can be emptied', async () => {
    // git's symlink-support probe: create `tXXXXXX -> testing` (dangling),
    // lstat it, unlink it, then remove the directory. unlink(2) must not
    // resolve the symlink leaf, and rmdir must not report EIO afterwards.
    ctx = await createIntegrationKernel();

    const probe = await ctx.kernel.exec(
      'sh -c "mkdir /tmp/probe-dir && ln -s missing-target /tmp/probe-dir/probe && rm /tmp/probe-dir/probe && rmdir /tmp/probe-dir"',
    );
    expect(probe.exitCode, probe.stderr).toBe(0);
    expect(await ctx.vfs.exists('/tmp/probe-dir')).toBe(false);
  });

  it('guest rmdir on a non-empty directory reports ENOTEMPTY, not EIO', async () => {
    ctx = await createIntegrationKernel();

    const setup = await ctx.kernel.exec(
      'sh -c "mkdir /tmp/full-dir && echo keep > /tmp/full-dir/keep.txt"',
    );
    expect(setup.exitCode).toBe(0);

    const rmdir = await ctx.kernel.exec('rmdir /tmp/full-dir');
    expect(rmdir.exitCode).not.toBe(0);
    expect(rmdir.stderr.toLowerCase()).toContain('not empty');
    expect(rmdir.stderr.toLowerCase()).not.toContain('i/o error');
    expect(await ctx.vfs.exists('/tmp/full-dir/keep.txt')).toBe(true);
  });
});
