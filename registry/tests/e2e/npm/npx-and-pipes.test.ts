/**
 * E2E test: npx execution and pipe-based Node eval through kernel.
 *
 * npx tests verify the npm/npx command resolution chain through the kernel
 * command registry. Pipe tests exercise the WasmVM -> kernel PipeManager -> Node
 * stdin path, complementing the unit-level pipe tests.
 */

import { describe, expect, it } from 'vitest';
import { createIntegrationKernel, skipUnlessWasmBuilt } from '../helpers.ts';

const skipReason = skipUnlessWasmBuilt();

/** Check if npm registry is reachable (5s timeout). */
async function checkNetwork(): Promise<string | false> {
  try {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 5_000);
    await fetch('https://registry.npmjs.org/', {
      signal: controller.signal,
      method: 'HEAD',
    });
    clearTimeout(timeout);
    return false;
  } catch {
    return 'network not available (cannot reach npm registry)';
  }
}

describe.skipIf(skipReason)('e2e npx and pipes through kernel', () => {
  describe('npx execution', () => {
    it('npx semver outputs parsed version', async () => {
      const networkSkip = await checkNetwork();
      if (networkSkip) {
        console.log(`Skipping npx test: ${networkSkip}`);
        return;
      }

      const { kernel, dispose } = await createIntegrationKernel({
        runtimes: ['wasmvm', 'node'],
      });

      try {
        const result = await kernel.exec('npx -y semver 1.2.3', { cwd: '/' });
        expect(result.stdout.trim()).toBe('1.2.3');
      } finally {
        await dispose();
      }
    }, 30_000);
  });

  describe('pipe-based Node eval', () => {
    it('echo piped to node -e evaluates expression', async () => {
      const { kernel, dispose } = await createIntegrationKernel({
        runtimes: ['wasmvm', 'node'],
      });

      try {
        const result = await kernel.exec(
          `echo 1+2 | node -e "process.stdin.on('data', d => console.log(eval(d.toString().trim())))"`,
          { cwd: '/' },
        );
        expect(result.stdout.trim()).toBe('3');
      } finally {
        await dispose();
      }
    }, 30_000);

    it('echo piped to node -e with end event transforms to uppercase', async () => {
      const { kernel, dispose } = await createIntegrationKernel({
        runtimes: ['wasmvm', 'node'],
      });

      try {
        const result = await kernel.exec(
          `echo hello world | node -e "let d=''; process.stdin.on('data',c=>d+=c); process.stdin.on('end',()=>console.log(d.trim().toUpperCase()))"`,
          { cwd: '/' },
        );
        expect(result.stdout.trim()).toBe('HELLO WORLD');
      } finally {
        await dispose();
      }
    }, 30_000);

    it('cat VFS file piped to node -e processes file content', async () => {
      const { kernel, dispose } = await createIntegrationKernel({
        runtimes: ['wasmvm', 'node'],
      });

      try {
        // Pre-write data file to VFS
        await kernel.mkdir('/tmp');
        await kernel.writeFile('/tmp/data.txt', 'hello from file');

        const result = await kernel.exec(
          `cat /tmp/data.txt | node -e "let d=''; process.stdin.on('data',c=>d+=c); process.stdin.on('end',()=>console.log(d.trim().toUpperCase()))"`,
          { cwd: '/' },
        );
        expect(result.stdout.trim()).toBe('HELLO FROM FILE');
      } finally {
        await dispose();
      }
    }, 30_000);
  });
});
