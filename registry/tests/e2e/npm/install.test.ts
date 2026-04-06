/**
 * E2E test: npm install a package through kernel.
 *
 * Verifies the full package installation flow:
 *   1. Write package.json with left-pad dependency to temp dir
 *   2. kernel.exec('npm install') downloads and extracts the package
 *   3. Installed package is usable via require() in kernel Node
 *
 * Uses NodeFileSystem rooted at a temp directory so npm's filesystem
 * operations (mkdir, symlink, writeFile) hit the real host filesystem.
 */

import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  COMMANDS_DIR,
  createKernel,
  NodeFileSystem,
  createWasmVmRuntime,
  createNodeRuntime,
  skipUnlessWasmBuilt,
} from '../helpers.ts';

const wasmSkip = skipUnlessWasmBuilt();

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

const skipReason = wasmSkip || (await checkNetwork());

describe.skipIf(skipReason)('e2e npm install through kernel', () => {
  it(
    'npm install installs left-pad and it is usable by node',
    async () => {
      const tempDir = await mkdtemp(
        path.join(tmpdir(), 'kernel-npm-install-'),
      );

      try {
        // Write minimal package.json to temp dir
        await writeFile(
          path.join(tempDir, 'package.json'),
          JSON.stringify({
            name: 'test-npm-install',
            private: true,
            dependencies: { 'left-pad': '1.3.0' },
          }),
        );

        // Kernel with NodeFileSystem rooted at temp dir
        const vfs = new NodeFileSystem({ root: tempDir });
        const kernel = createKernel({ filesystem: vfs, cwd: '/' });

        await kernel.mount(
          createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
        );
        await kernel.mount(createNodeRuntime());

        try {
          // Run npm install through kernel
          const installResult = await kernel.exec('npm install', {
            cwd: '/',
          });
          expect(installResult.exitCode).toBe(0);

          // Verify node_modules/left-pad/ exists in the VFS
          const stat = await vfs.stat('/node_modules/left-pad');
          expect(stat.isDirectory).toBe(true);

          // Verify installed package is usable via require()
          const result = await kernel.exec(
            `node -e "console.log(require('left-pad')('hi', 10))"`,
            { cwd: '/' },
          );
          expect(result.stdout.trim()).toBe('        hi');
        } finally {
          await kernel.dispose();
        }
      } finally {
        await rm(tempDir, { recursive: true, force: true });
      }
    },
    30_000,
  );
});
