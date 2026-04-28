/**
 * E2E test: Next.js build through kernel.
 *
 * Verifies that 'next build' completes through the kernel on the repo-owned
 * Next.js fixture, proving the kernel can handle a complex real-world
 * build pipeline:
 *   1. Host-side package install populates node_modules
 *   2. NodeFileSystem mounts the project into the kernel
 *   3. kernel.exec('npx next build') runs Next.js through kernel
 *   4. Build output directory exists after completion
 *
 * Known workarounds applied:
 *   - NEXT_DISABLE_SWC=1: SWC is a native .node addon that the sandbox
 *     blocks (ERR_MODULE_ACCESS_NATIVE_ADDON), so we force Babel fallback
 *   - The checked-in fixture writes normal Next.js build output to `.next`
 */

import { cp, mkdtemp, rm } from 'node:fs/promises';
import { execSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
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
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const NEXTJS_FIXTURE_DIR = path.resolve(__dirname, '../projects/nextjs-pass');

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

describeIf(!skipReason, 'e2e Next.js build through kernel', () => {
  let tempDir: string;

  // Copy the checked-in fixture so the build can mutate /.next without touching the repo.
  beforeAll(async () => {
    tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-nextjs-build-'));
    await cp(NEXTJS_FIXTURE_DIR, tempDir, { recursive: true });

    // Match the registry fixture install path instead of doing a slow ad hoc npm install.
    execSync('pnpm install --ignore-workspace --prefer-offline', {
      cwd: tempDir,
      stdio: 'pipe',
      timeout: 60_000,
    });
  }, 90_000);

  afterAll(async () => {
    if (tempDir) {
      await rm(tempDir, { recursive: true, force: true });
    }
  });

  it(
    'next build produces output directory',
    async () => {
      const vfs = new NodeFileSystem({ root: tempDir });
      const kernel = createKernel({ filesystem: vfs, cwd: '/' });

      await kernel.mount(
        createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
      );
      await kernel.mount(createNodeRuntime());

      try {
        const result = await kernel.exec('npx next build', {
          cwd: '/',
          env: {
            // Disable SWC. Native .node addon blocked by sandbox.
            NEXT_DISABLE_SWC: '1',
            // Force single-threaded. worker_threads not supported in V8 isolate.
            NEXT_EXPERIMENTAL_WORKERS: '0',
            // Suppress telemetry
            NEXT_TELEMETRY_DISABLED: '1',
          },
        });

        expect(result.exitCode).toBe(0);

        // Some fixtures may emit a static export, but the checked-in Next.js
        // kernel fixture currently writes its build artifacts to `.next`.
        const outExists = await vfs
          .stat('/out')
          .then(() => true)
          .catch(() => false);

        // Fallback: check .next/ if out/ doesn't exist (non-export mode)
        const dotNextExists = await vfs
          .stat('/.next')
          .then(() => true)
          .catch(() => false);

        expect(outExists || dotNextExists).toBe(true);
      } finally {
        await kernel.dispose();
      }
    },
    120_000,
  );
});
