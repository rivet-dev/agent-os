import { afterEach, describe, expect, it } from 'vitest';
import {
  createIntegrationKernel,
  describeIf,
  skipUnlessWasmBuilt,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const skipReason = skipUnlessWasmBuilt();

describeIf(!skipReason, 'WASM shim command smoke', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) {
      await ctx.dispose();
    }
  });

  it('nohup and stdbuf execute guest shell commands through WasmVM', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });

    const nohupResult = await ctx.kernel.exec("nohup sh -c 'printf alpha'");
    expect(nohupResult.exitCode).toBe(0);
    expect(nohupResult.stdout).toBe('alpha');

    const stdbufResult = await ctx.kernel.exec("stdbuf -oL sh -c 'printf beta'");
    expect(stdbufResult.exitCode).toBe(0);
    expect(stdbufResult.stdout).toBe('beta');
  });
});
