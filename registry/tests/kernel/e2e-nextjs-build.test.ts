/**
 * E2E test: Next.js build through kernel.
 *
 * Verifies that 'next build' completes through the kernel on a minimal
 * Next.js project, proving the kernel can handle a complex real-world
 * build pipeline:
 *   1. Host-side npm install populates node_modules
 *   2. NodeFileSystem mounts the project into the kernel
 *   3. kernel.exec('npx next build') runs Next.js through kernel
 *   4. Build output directory exists after completion
 *
 * Known workarounds applied:
 *   - NEXT_DISABLE_SWC=1: SWC is a native .node addon that the sandbox
 *     blocks (ERR_MODULE_ACCESS_NATIVE_ADDON), so we force Babel fallback
 *   - output:'export' in next.config: produces static output for simpler build
 */

import { mkdtemp, rm, writeFile, mkdir } from 'node:fs/promises';
import { execSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import {
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

describe.skipIf(skipReason)('e2e Next.js build through kernel', () => {
  let tempDir: string;

  // Set up minimal Next.js project and install dependencies on host
  beforeAll(async () => {
    tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-nextjs-build-'));

    // Minimal package.json with Next.js
    await writeFile(
      path.join(tempDir, 'package.json'),
      JSON.stringify({
        name: 'test-nextjs-build',
        private: true,
        dependencies: {
          next: '^14',
          react: '^18',
          'react-dom': '^18',
        },
      }),
    );

    // next.config.js with static export mode
    await writeFile(
      path.join(tempDir, 'next.config.js'),
      `/** @type {import('next').NextConfig} */
module.exports = {
  output: 'export',
};
`,
    );

    // Minimal App Router structure
    await mkdir(path.join(tempDir, 'app'), { recursive: true });

    // Root layout (required by App Router)
    await writeFile(
      path.join(tempDir, 'app', 'layout.tsx'),
      `export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
`,
    );

    // Simple page component
    await writeFile(
      path.join(tempDir, 'app', 'page.tsx'),
      `export default function Home() {
  return <h1>Hello World</h1>;
}
`,
    );

    // Host-side npm install to populate node_modules
    execSync('npm install --ignore-scripts', {
      cwd: tempDir,
      stdio: 'pipe',
      timeout: 120_000,
    });
  }, 180_000);

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

        // Static export mode writes to out/ directory
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
