/**
 * Cross-runtime error and exit code propagation tests.
 *
 * Verifies that exit codes and stderr flow correctly across runtime
 * boundaries, including through nested cross-runtime spawns (e.g.
 * WasmVM shell -> kernel -> Node -> exit(42) -> kernel -> WasmVM).
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

describe.skipIf(skipReason)('cross-runtime error propagation', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('Node non-zero exit propagates', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "process.exit(42)"');
    expect(result.exitCode).toBe(42);
  });

  it('WasmVM non-zero exit propagates', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    const result = await ctx.kernel.exec('sh -c "exit 7"');
    expect(result.exitCode).toBe(7);
  });

  it('Node stderr captured by kernel', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "console.error(\'err-msg\')"');
    expect(result.stderr).toContain('err-msg');
  });

  it('WasmVM stderr captured by kernel', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    const result = await ctx.kernel.exec('echo error >&2');
    expect(result.stderr).toContain('error');
  });

  it('nested cross-runtime exit code propagation', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    // WasmVM shell spawns Node via kernel, Node exits 42, shell propagates
    // Tests exit code flowing through TWO runtime boundary crossings:
    // kernel->WasmVM->kernel->Node->exit(42)->kernel->WasmVM->kernel
    const result = await ctx.kernel.exec('sh -c "node -e \\"process.exit(42)\\""');
    expect(result.exitCode).toBe(42);
  });

  it('Node captures WasmVM child failure', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    // Node runs child_process.execSync which routes through kernel to WasmVM shell
    // The shell exits 3, Node catches the error and logs the exit code
    const code = [
      'try {',
      '  require("child_process").execSync("sh -c \\"exit 3\\"");',
      '} catch (e) {',
      '  console.log(e.status);',
      '}',
    ].join(' ');
    const result = await ctx.kernel.exec(`node -e '${code}'`);
    expect(result.stdout).toContain('3');
  });
});
