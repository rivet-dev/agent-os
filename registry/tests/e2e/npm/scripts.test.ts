/**
 * E2E test: npm run scripts execute shell commands through kernel.
 *
 * Exercises the full round-trip: kernel.exec('npm run greet')
 *   -> sh -c 'npm run greet'  (WasmVM shell)
 *   -> proc_spawn('npm', ...)  (kernel routes to Node driver)
 *   -> npm reads package.json, spawns 'sh -c "echo hello world"'
 *   -> child_process routes through kernel -> WasmVM shell -> output
 */

import { describe, expect, it } from 'vitest';
import { createIntegrationKernel, skipUnlessWasmBuilt } from '../helpers.ts';

const skipReason = skipUnlessWasmBuilt();

describe.skipIf(skipReason)('e2e npm run scripts through kernel', () => {
  it('npm run greet echoes hello world', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.writeFile(
        '/package.json',
        JSON.stringify({
          name: 'test-npm-scripts',
          scripts: { greet: 'echo hello world' },
        }),
      );

      const result = await kernel.exec('npm run greet', { cwd: '/' });
      expect(result.stdout).toContain('hello world');
    } finally {
      await dispose();
    }
  }, 30_000);

  it('npm run count runs sequential shell commands (&&)', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.writeFile(
        '/package.json',
        JSON.stringify({
          name: 'test-npm-scripts',
          scripts: { count: 'echo one && echo two && echo three' },
        }),
      );

      const result = await kernel.exec('npm run count', { cwd: '/' });
      expect(result.stdout).toContain('one');
      expect(result.stdout).toContain('two');
      expect(result.stdout).toContain('three');
    } finally {
      await dispose();
    }
  }, 30_000);

  it('npm run env-check passes npm env variables through shell', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.writeFile(
        '/package.json',
        JSON.stringify({
          name: 'my-cool-project',
          scripts: { 'env-check': 'echo $npm_package_name' },
        }),
      );

      const result = await kernel.exec('npm run env-check', { cwd: '/' });
      expect(result.stdout).toContain('my-cool-project');
    } finally {
      await dispose();
    }
  }, 30_000);

  it('npm run nonexistent returns non-zero exit code', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.writeFile(
        '/package.json',
        JSON.stringify({
          name: 'test-npm-scripts',
          scripts: {},
        }),
      );

      const result = await kernel.exec('npm run nonexistent', { cwd: '/' });
      expect(result.exitCode).not.toBe(0);
    } finally {
      await dispose();
    }
  }, 30_000);
});
