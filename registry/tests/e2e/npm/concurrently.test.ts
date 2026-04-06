/**
 * E2E test: concurrently package runs parallel processes through kernel.
 *
 * Verifies that concurrently spawns multiple child processes simultaneously
 * through the kernel, testing:
 *   1. Kernel process table concurrent PID allocation without conflicts
 *   2. Kernel command registry handling multiple simultaneous resolves
 *   3. WasmVM running multiple workers in parallel
 *   4. Kernel pipe multiplexing stdout from multiple processes
 *
 * Pre-installs concurrently on host via npm, then mounts NodeFileSystem
 * so the kernel finds the binary in node_modules/.bin/.
 */

import { mkdtemp, rm, writeFile } from 'node:fs/promises';
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

describe.skipIf(skipReason)('e2e concurrently through kernel', () => {
  let tempDir: string;

  // Pre-install concurrently on host so kernel has node_modules available
  beforeAll(async () => {
    tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-concurrently-'));

    await writeFile(
      path.join(tempDir, 'package.json'),
      JSON.stringify({
        name: 'test-concurrently',
        private: true,
        dependencies: { concurrently: '^8.0.0' },
      }),
    );

    execSync('npm install --ignore-scripts', {
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

  /** Create kernel with NodeFileSystem rooted at temp dir. */
  async function createConcurrentlyKernel() {
    const vfs = new NodeFileSystem({ root: tempDir });
    const kernel = createKernel({ filesystem: vfs, cwd: '/' });

    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
    );
    await kernel.mount(createNodeRuntime());

    return { kernel, dispose: () => kernel.dispose() };
  }

  it(
    'concurrently runs two echo commands in parallel',
    async () => {
      const { kernel, dispose } = await createConcurrentlyKernel();

      try {
        const result = await kernel.exec(
          'npx concurrently "echo hello" "echo world"',
          { cwd: '/' },
        );
        expect(result.stdout).toContain('hello');
        expect(result.stdout).toContain('world');
      } finally {
        await dispose();
      }
    },
    30_000,
  );

  it(
    'concurrently runs processes with shell operators',
    async () => {
      const { kernel, dispose } = await createConcurrentlyKernel();

      try {
        const result = await kernel.exec(
          'npx concurrently "echo one && echo two" "echo three"',
          { cwd: '/' },
        );
        expect(result.stdout).toContain('one');
        expect(result.stdout).toContain('two');
        expect(result.stdout).toContain('three');
      } finally {
        await dispose();
      }
    },
    30_000,
  );

  it(
    'concurrently --kill-others-on-fail returns non-zero on child failure',
    async () => {
      const { kernel, dispose } = await createConcurrentlyKernel();

      try {
        const result = await kernel.exec(
          'npx concurrently --kill-others-on-fail "echo success" "exit 1"',
          { cwd: '/' },
        );
        expect(result.exitCode).not.toBe(0);
      } finally {
        await dispose();
      }
    },
    30_000,
  );

  it(
    'concurrent process spawns get unique PIDs',
    async () => {
      const { kernel, dispose } = await createConcurrentlyKernel();

      try {
        // Spawn multiple processes concurrently through kernel
        const procs = [
          kernel.spawn('echo', ['pid-a'], { cwd: '/' }),
          kernel.spawn('echo', ['pid-b'], { cwd: '/' }),
          kernel.spawn('echo', ['pid-c'], { cwd: '/' }),
        ];

        const pids = procs.map((p) => p.pid);
        const uniquePids = new Set(pids);
        expect(uniquePids.size).toBe(3);

        // Wait for all to complete
        await Promise.all(procs.map((p) => p.wait()));
      } finally {
        await dispose();
      }
    },
    30_000,
  );
});
