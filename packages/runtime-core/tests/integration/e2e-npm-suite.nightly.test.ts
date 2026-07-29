/**
 * E2E test suite: npm operations through kernel.
 *
 * Covers the core npm workflow: init, install, list, run, npx.
 * Tests are split into offline (no network) and online (network-dependent)
 * sections, with network availability guarded by a registry check.
 *
 * Known limitation: npm commands that trigger the update-notifier / pacote /
 * @sigstore/sign module chain fail in the V8 isolate sandbox because
 * http2.constants is not yet polyfilled. This affects npm install, npm init -y,
 * and npx. These tests are guarded and will pass once http2 bridge support
 * is added.
 */

import { existsSync } from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { expect, it } from 'vitest';
import {
  describeIf,
  COMMANDS_DIR,
  createKernel,
  createWasmVmRuntime,
  createNodeRuntime,
  createIntegrationKernel,
  NodeFileSystem,
  skipUnlessWasmBuilt,
} from '@rivet-dev/agentos-vm-test-harness';

const wasmSkip = skipUnlessWasmBuilt();
const ONLINE_NPM_COMMAND_TEST_TIMEOUT_MS = 60_000;
const ONLINE_NPM_COMPOUND_TEST_TIMEOUT_MS = 90_000;
const ONLINE_NPX_TEST_TIMEOUT_MS = 120_000;

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

/**
 * Check if npm install works in the kernel sandbox.
 * npm's pacote -> @sigstore/sign chain requires http2.constants which is not
 * yet polyfilled. Returns a skip reason if npm install is broken.
 */
async function checkNpmInstallWorks(): Promise<string | false> {
  if (wasmSkip) return wasmSkip;
  const tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-npm-probe-'));
  try {
    await writeFile(
      path.join(tempDir, 'package.json'),
      JSON.stringify({
        name: 'npm-probe',
        private: true,
        dependencies: { 'left-pad': '1.3.0' },
      }),
    );
    const vfs = new NodeFileSystem({ root: tempDir });
    const kernel = createKernel({ filesystem: vfs, cwd: '/' });
    await kernel.mount(
      createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
    );
    await kernel.mount(createNodeRuntime());
    try {
      const result = await kernel.exec('npm install', { cwd: '/' });
      if (existsSync(path.join(tempDir, 'node_modules', 'left-pad'))) {
        return false;
      }
      return 'npm install fails in sandbox (http2/@sigstore/sign not polyfilled)';
    } finally {
      await kernel.dispose();
    }
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
}

// --- Offline tests (no network required) ---

// TODO(P6): npm command suite depends on registry command/runtime artifacts.
describeIf(process.env.AGENTOS_NPM_WORKFLOWS_E2E === '1', 'npm suite - offline', () => {
  it('npm init -y creates package.json with default values', async () => {
    const tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-npm-init-'));

    try {
      const vfs = new NodeFileSystem({ root: tempDir });
      const kernel = createKernel({ filesystem: vfs, cwd: '/' });

      await kernel.mount(
        createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
      );
      await kernel.mount(createNodeRuntime());

      try {
        await kernel.exec('npm init -y', { cwd: '/' });

        const exists = await vfs.exists('/package.json');
        expect(exists).toBe(true);

        const content = await vfs.readTextFile('/package.json');
        const pkg = JSON.parse(content);
        expect(pkg).toHaveProperty('name');
        expect(pkg).toHaveProperty('version');
        expect(pkg.version).toBe('1.0.0');
      } finally {
        await kernel.dispose();
      }
    } finally {
      await rm(tempDir, { recursive: true, force: true });
    }
  }, 30_000);

  it('npm list shows installed packages (empty project)', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.writeFile(
        '/package.json',
        JSON.stringify({
          name: 'test-npm-list',
          version: '1.0.0',
          private: true,
        }),
      );

      const result = await kernel.exec('npm list', { cwd: '/' });
      expect(result.stdout).toContain('test-npm-list');
    } finally {
      await dispose();
    }
  }, 30_000);

  it('npm run test executes script from package.json', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.writeFile(
        '/package.json',
        JSON.stringify({
          name: 'test-npm-run',
          scripts: { test: 'echo npm-test-output' },
        }),
      );

      const result = await kernel.exec('npm run test', { cwd: '/' });
      expect(result.stdout).toContain('npm-test-output');
    } finally {
      await dispose();
    }
  }, 30_000);

  it('npm run with missing script shows error and hint', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.writeFile(
        '/package.json',
        JSON.stringify({
          name: 'test-npm-run-missing',
          scripts: {
            build: 'echo building',
            start: 'echo starting',
          },
        }),
      );

      const result = await kernel.exec('npm run nonexistent', { cwd: '/' });
      expect(result.exitCode).not.toBe(0);

      // npm reports "Missing script" and suggests running "npm run" to list scripts
      const output = result.stdout + result.stderr;
      expect(output).toMatch(/Missing script/i);
      expect(output).toContain('npm run');
    } finally {
      await dispose();
    }
  }, 30_000);
});

// --- Online tests (require network + working npm install) ---

const networkSkip = await checkNetwork();
const npmInstallSkip = wasmSkip || networkSkip || (await checkNpmInstallWorks());
void npmInstallSkip;

// TODO(P6): npm install suite depends on registry/network package resolution.
describeIf(process.env.AGENTOS_NPM_WORKFLOWS_E2E === '1', 'npm suite - online', () => {
  it(
    'npm install left-pad installs package to node_modules',
    async () => {
      const tempDir = await mkdtemp(
        path.join(tmpdir(), 'kernel-npm-install-suite-'),
      );

      try {
        await writeFile(
          path.join(tempDir, 'package.json'),
          JSON.stringify({
            name: 'test-npm-install-suite',
            private: true,
            dependencies: { 'left-pad': '1.3.0' },
          }),
        );

        const vfs = new NodeFileSystem({ root: tempDir });
        const kernel = createKernel({ filesystem: vfs, cwd: '/' });

        await kernel.mount(
          createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
        );
        await kernel.mount(createNodeRuntime());

        try {
          const installResult = await kernel.exec('npm install', {
            cwd: '/',
          });

          // Verify node_modules/left-pad/ exists
          const stat = await vfs.stat('/node_modules/left-pad');
          expect(stat.isDirectory).toBe(true);
        } finally {
          await kernel.dispose();
        }
      } finally {
        await rm(tempDir, { recursive: true, force: true });
      }
    },
    ONLINE_NPM_COMMAND_TEST_TIMEOUT_MS,
  );

  it(
    'npm list shows installed packages after install',
    async () => {
      const tempDir = await mkdtemp(
        path.join(tmpdir(), 'kernel-npm-list-suite-'),
      );

      try {
        await writeFile(
          path.join(tempDir, 'package.json'),
          JSON.stringify({
            name: 'test-npm-list-suite',
            private: true,
            dependencies: { 'left-pad': '1.3.0' },
          }),
        );

        const vfs = new NodeFileSystem({ root: tempDir });
        const kernel = createKernel({ filesystem: vfs, cwd: '/' });

        await kernel.mount(
          createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
        );
        await kernel.mount(createNodeRuntime());

        try {
          // Install first
          await kernel.exec('npm install', { cwd: '/' });

          // npm list should show left-pad
          const listResult = await kernel.exec('npm list', { cwd: '/' });
          expect(listResult.stdout).toContain('left-pad');
        } finally {
          await kernel.dispose();
        }
      } finally {
        await rm(tempDir, { recursive: true, force: true });
      }
    },
    ONLINE_NPM_COMPOUND_TEST_TIMEOUT_MS,
  );

  it(
    'npx -y cowsay hello runs cowsay without prior install',
    async () => {
      const { kernel, dispose } = await createIntegrationKernel({
        runtimes: ['wasmvm', 'node'],
      });

      try {
        const result = await kernel.exec('npx -y cowsay hello', { cwd: '/' });
        expect(result.stdout).toContain('hello');
      } finally {
        await dispose();
      }
    },
    // A cold npx invocation downloads and extracts the full package graph before
    // launching its bin. Keep this online-only case bounded without applying the
    // larger allowance to the cheap npm workflow tests.
    ONLINE_NPX_TEST_TIMEOUT_MS,
  );
});

// --- Error handling ---

// TODO(P6): npm error-path suite depends on registry command/runtime artifacts.
describeIf(process.env.AGENTOS_NPM_WORKFLOWS_E2E === '1', 'npm suite - error handling', () => {
  it(
    'npm install with unreachable registry returns clear error',
    async () => {
      const { kernel, dispose } = await createIntegrationKernel({
        runtimes: ['wasmvm', 'node'],
      });

      try {
        await kernel.writeFile(
          '/package.json',
          JSON.stringify({
            name: 'test-npm-no-network',
            private: true,
            dependencies: { 'left-pad': '1.3.0' },
          }),
        );

        // Use an unreachable registry to simulate no network
        const result = await kernel.exec(
          [
            'npm install',
            '--registry=http://127.0.0.1:1',
            '--fetch-retries=0',
            '--fetch-timeout=1000',
            '--fetch-retry-mintimeout=1',
            '--fetch-retry-maxtimeout=1',
          ].join(' '),
          { cwd: '/' },
        );
        expect(result.exitCode).not.toBe(0);

        const output = result.stdout + result.stderr;
        expect(output).toMatch(/ERR|error|ECONNREFUSED|fetch failed/i);
      } finally {
        await dispose();
      }
    },
    ONLINE_NPM_COMMAND_TEST_TIMEOUT_MS,
  );
});
