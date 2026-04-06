/**
 * Module resolution through kernel VFS tests.
 *
 * Verifies that Node's require() resolves modules via the kernel VFS,
 * enabling CJS modules, node_modules, nested paths, and JSON imports.
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

describe.skipIf(skipReason)('module resolution through kernel VFS', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('require relative CJS module', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    await ctx.kernel.writeFile(
      '/app/lib/utils.js',
      'module.exports.add = (a, b) => a + b;',
    );
    await ctx.kernel.writeFile(
      '/app/index.js',
      "console.log(require('./lib/utils').add(1, 2));",
    );

    const result = await ctx.kernel.exec('node /app/index.js');
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('3');
  });

  it('require from node_modules', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    await ctx.kernel.writeFile(
      '/app/node_modules/my-pkg/index.js',
      "module.exports.greet = () => 'hello';",
    );
    await ctx.kernel.writeFile(
      '/app/main.js',
      "console.log(require('my-pkg').greet());",
    );

    const result = await ctx.kernel.exec('node /app/main.js');
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('hello');
  });

  it('require missing module gives MODULE_NOT_FOUND', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // Ensure /app exists as cwd
    await ctx.kernel.writeFile('/app/.keep', '');

    const result = await ctx.kernel.exec(
      `node -e "require('./nonexistent')"`,
      { cwd: '/app' },
    );
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toMatch(/Cannot find module|MODULE_NOT_FOUND/);
  });

  it('require nested relative path', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    await ctx.kernel.writeFile(
      '/app/lib/utils.js',
      'module.exports.add = (a, b) => a + b;',
    );
    await ctx.kernel.writeFile(
      '/app/src/handlers/index.js',
      "console.log(require('../../lib/utils').add(3, 4));",
    );

    const result = await ctx.kernel.exec('node /app/src/handlers/index.js');
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('7');
  });

  it('require JSON file', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    await ctx.kernel.writeFile(
      '/app/config.json',
      JSON.stringify({ port: 3000 }),
    );

    const result = await ctx.kernel.exec(
      `node -e "console.log(require('./config.json').port)"`,
      { cwd: '/app' },
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('3000');
  });
});
