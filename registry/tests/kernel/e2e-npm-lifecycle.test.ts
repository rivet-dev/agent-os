/**
 * E2E test: npm postinstall lifecycle scripts through kernel.
 *
 * Verifies that npm lifecycle hooks (preinstall, postinstall) route through
 * the kernel command registry to WasmVM shell:
 *   1. npm install reads package.json lifecycle scripts
 *   2. npm spawns 'sh -c "echo ..."' for each lifecycle hook
 *   3. child_process.spawn routes through kernel -> WasmVM shell
 *   4. Shell commands write marker files to kernel VFS
 *
 * Uses NodeFileSystem rooted at a temp directory so npm's filesystem
 * operations hit the real host filesystem.
 */

import { mkdtemp, rm, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  describeIf,
  COMMANDS_DIR,
  createKernel,
  NodeFileSystem,
  createWasmVmRuntime,
  createNodeRuntime,
  skipUnlessWasmBuilt,
} from './helpers.ts';

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

describeIf(!skipReason, 'e2e npm lifecycle scripts through kernel', () => {
  it(
    'postinstall script writes marker file during npm install',
    async () => {
      const tempDir = await mkdtemp(
        path.join(tmpdir(), 'kernel-npm-lifecycle-'),
      );

      try {
        // Create /tmp inside the project root so lifecycle scripts can write there
        await mkdir(path.join(tempDir, 'tmp'), { recursive: true });

        // Package.json with postinstall lifecycle script
        await writeFile(
          path.join(tempDir, 'package.json'),
          JSON.stringify({
            name: 'test-npm-lifecycle',
            private: true,
            scripts: {
              postinstall: 'echo POSTINSTALL_RAN > /tmp/marker.txt',
            },
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
          const result = await kernel.exec('npm install', { cwd: '/' });
          expect(result.exitCode).toBe(0);

          // Verify postinstall marker was written through WasmVM shell
          const markerBytes = await vfs.readFile('/tmp/marker.txt');
          const marker = new TextDecoder().decode(markerBytes).trim();
          expect(marker).toBe('POSTINSTALL_RAN');
        } finally {
          await kernel.dispose();
        }
      } finally {
        await rm(tempDir, { recursive: true, force: true });
      }
    },
    45_000,
  );

  it(
    'preinstall script writes marker file before dependencies are fetched',
    async () => {
      const tempDir = await mkdtemp(
        path.join(tmpdir(), 'kernel-npm-lifecycle-pre-'),
      );

      try {
        // Create /tmp inside the project root
        await mkdir(path.join(tempDir, 'tmp'), { recursive: true });

        // Package.json with preinstall lifecycle script
        await writeFile(
          path.join(tempDir, 'package.json'),
          JSON.stringify({
            name: 'test-npm-lifecycle-pre',
            private: true,
            scripts: {
              preinstall: 'echo PREINSTALL_RAN > /tmp/pre-marker.txt',
            },
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
          const result = await kernel.exec('npm install', { cwd: '/' });
          expect(result.exitCode).toBe(0);

          // Verify preinstall marker was written through WasmVM shell
          const markerBytes = await vfs.readFile('/tmp/pre-marker.txt');
          const marker = new TextDecoder().decode(markerBytes).trim();
          expect(marker).toBe('PREINSTALL_RAN');
        } finally {
          await kernel.dispose();
        }
      } finally {
        await rm(tempDir, { recursive: true, force: true });
      }
    },
    45_000,
  );
});
