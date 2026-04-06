/**
 * Cross-runtime integration tests for kernel.exec() and kernel.spawn().
 *
 * Exercises real WasmVM driver end-to-end: shell parsing, coreutils
 * execution, VFS reads/writes, and error handling. Each test creates
 * a fresh kernel. No shared state between tests.
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

describe.skipIf(skipReason)('kernel.exec() integration', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('exec echo returns stdout', async () => {
    ctx = await createIntegrationKernel();
    const result = await ctx.kernel.exec('echo hello');
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('hello');
  });

  it('exec ls lists files in directory', async () => {
    ctx = await createIntegrationKernel();
    // Write a file into VFS, then list the directory
    await ctx.vfs.writeFile('/tmp/test-file.txt', 'content');
    const result = await ctx.kernel.exec('ls /tmp');
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('test-file.txt');
  });

  it('exec cat reads file contents', async () => {
    ctx = await createIntegrationKernel();
    await ctx.vfs.writeFile('/tmp/test.txt', 'hello from vfs');
    const result = await ctx.kernel.exec('cat /tmp/test.txt');
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('hello from vfs');
  });

  it('exec nonexistent command returns non-zero', async () => {
    ctx = await createIntegrationKernel();
    const result = await ctx.kernel.exec('nonexistent-command-xyz');
    expect(result.exitCode).not.toBe(0);
  });

  it('exec with env passes environment to process', async () => {
    ctx = await createIntegrationKernel();
    const result = await ctx.kernel.exec('echo $MY_VAR', {
      env: { MY_VAR: 'kernel-test' },
    });
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('kernel-test');
  });

  it('exec pipeline with pipe operator', async () => {
    ctx = await createIntegrationKernel();
    await ctx.vfs.writeFile('/tmp/data.txt', 'foo\nbar\nbaz\n');
    const result = await ctx.kernel.exec('cat /tmp/data.txt | wc -l');
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('3');
  });

  it('exec writes to VFS via redirection', async () => {
    ctx = await createIntegrationKernel();
    const result = await ctx.kernel.exec('echo "written by shell" > /tmp/out.txt');
    expect(result.exitCode).toBe(0);
    const content = await ctx.vfs.readFile('/tmp/out.txt');
    expect(new TextDecoder().decode(content).trim()).toBe('written by shell');
  });

  it('exec stderr captured on error', async () => {
    ctx = await createIntegrationKernel();
    const result = await ctx.kernel.exec('cat /nonexistent/path');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr.length).toBeGreaterThan(0);
  });
});

describe.skipIf(skipReason)('kernel.spawn() integration', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('spawn returns ManagedProcess with PID', async () => {
    ctx = await createIntegrationKernel();
    const proc = ctx.kernel.spawn('echo', ['spawn-test']);
    expect(proc.pid).toBeGreaterThan(0);
    const exitCode = await proc.wait();
    expect(exitCode).toBe(0);
  });

  it('spawn stdout fires onData via options callback', async () => {
    ctx = await createIntegrationKernel();
    const chunks: string[] = [];
    const proc = ctx.kernel.spawn('echo', ['spawn-output'], {
      onStdout: (data) => chunks.push(new TextDecoder().decode(data)),
    });
    await proc.wait();
    const output = chunks.join('');
    expect(output.trim()).toBe('spawn-output');
  });

  it('spawn exitCode resolves', async () => {
    ctx = await createIntegrationKernel();
    const proc = ctx.kernel.spawn('false', []);
    const exitCode = await proc.wait();
    expect(exitCode).not.toBe(0);
  });

  it('each test gets a fresh kernel with independent state', async () => {
    ctx = await createIntegrationKernel();
    // Write a file in this test's VFS
    await ctx.vfs.writeFile('/tmp/isolation-check.txt', 'exists');
    const result = await ctx.kernel.exec('cat /tmp/isolation-check.txt');
    expect(result.exitCode).toBe(0);

    // Create a second kernel. It should NOT see the first kernel's file.
    const ctx2 = await createIntegrationKernel();
    const result2 = await ctx2.kernel.exec('cat /tmp/isolation-check.txt');
    expect(result2.exitCode).not.toBe(0);
    await ctx2.dispose();
  });
});
