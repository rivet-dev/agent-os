/**
 * Cross-runtime pipe tests.
 *
 * Tests kernel pipe infrastructure: FD allocation, pipe read/write,
 * SpawnOptions FD overrides, cross-driver data flow, and EOF propagation.
 *
 * Integration tests with real WasmVM+Node are skipped when WASM binary
 * is not built.
 *
 * NOTE: The kernel-level unit tests (MockRuntimeDriver, no WASM) are kept
 * in the legacy runtime repo. Only the WasmVM-dependent integration tests
 * are included here.
 */

import { describe, it, expect, afterEach } from 'vitest';
import {
  describeIf,
  createIntegrationKernel,
  skipUnlessWasmBuilt,
} from './helpers.ts';
import type { Kernel } from './helpers.ts';

// ---------------------------------------------------------------------------
// Integration tests with real WasmVM + Node (skipped if WASM not built)
// ---------------------------------------------------------------------------

describeIf(!skipUnlessWasmBuilt(), 'cross-runtime pipes (WasmVM + Node)', () => {
  let kernel: Kernel;
  let dispose: () => Promise<void>;

  afterEach(async () => {
    await dispose?.();
  });

  it('WasmVM echo | cat pipe works', async () => {
    ({ kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm'] }));
    const result = await kernel.exec('echo hello | cat', { timeout: 15000 });
    expect(result.stdout.trim()).toBe('hello');
    expect(result.exitCode).toBe(0);
  }, 30000);

  it('WasmVM echo | node -e pipe works', async () => {
    ({ kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] }));
    const script = 'let d="";process.stdin.on("data",c=>d+=c);process.stdin.on("end",()=>process.stdout.write(d.toUpperCase()))';
    const result = await kernel.exec(`echo hello | node -e '${script}'`, { timeout: 15000 });
    expect(result.stdout.trim()).toBe('HELLO');
    expect(result.exitCode).toBe(0);
  }, 30000);

  it('WasmVM echo | WasmVM wc -c pipe works', async () => {
    ({ kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm'] }));
    const result = await kernel.exec('echo hello | wc -c', { timeout: 15000 });
    // "hello\n" is 6 bytes
    expect(result.stdout.trim()).toBe('6');
    expect(result.exitCode).toBe(0);
  }, 30000);
});
