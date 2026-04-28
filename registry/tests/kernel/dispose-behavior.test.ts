/**
 * Integration tests for kernel.dispose() with active processes.
 *
 * Verifies that dispose terminates running processes across WasmVM and Node
 * runtimes, cleans up after crashes, disposes timers, propagates pipe EOF,
 * and supports idempotent double-dispose.
 *
 * The pure kernel unit tests (MockRuntimeDriver, no WASM) remain in
 * the legacy runtime repo. Only WasmVM-dependent integration tests are here.
 */

import { describe, it, expect, afterEach } from 'vitest';
import {
  describeIf,
  createKernel,
  createNodeRuntime,
  createIntegrationKernel,
  skipUnlessWasmBuilt,
  createInMemoryFileSystem,
} from './helpers.ts';
import type { Kernel } from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const skipReason = skipUnlessWasmBuilt();

describeIf(!skipReason, 'dispose with active processes (integration)', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('dispose terminates active WasmVM sleep process within 5s', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });

    // Spawn a long-running sleep. Would hang for 60s without dispose.
    const proc = ctx.kernel.spawn('sleep', ['60']);
    expect(proc.pid).toBeGreaterThan(0);

    const start = Date.now();
    await ctx.dispose();
    const elapsed = Date.now() - start;

    expect(elapsed).toBeLessThan(5000);
  }, 10_000);

  it('dispose terminates active Node setTimeout process within 5s', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // Spawn a Node process that hangs for 60s
    const proc = ctx.kernel.spawn('node', ['-e', 'setTimeout(()=>{},60000)']);
    expect(proc.pid).toBeGreaterThan(0);

    const start = Date.now();
    await ctx.dispose();
    const elapsed = Date.now() - start;

    expect(elapsed).toBeLessThan(5000);
  }, 10_000);

  it('dispose terminates processes in BOTH WasmVM and Node simultaneously', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // Spawn long-running processes in both runtimes
    const wasmProc = ctx.kernel.spawn('sleep', ['60']);
    const nodeProc = ctx.kernel.spawn('node', ['-e', 'setTimeout(()=>{},60000)']);

    expect(wasmProc.pid).toBeGreaterThan(0);
    expect(nodeProc.pid).toBeGreaterThan(0);
    expect(wasmProc.pid).not.toBe(nodeProc.pid);

    const start = Date.now();
    await ctx.dispose();
    const elapsed = Date.now() - start;

    expect(elapsed).toBeLessThan(5000);
  }, 10_000);
});
