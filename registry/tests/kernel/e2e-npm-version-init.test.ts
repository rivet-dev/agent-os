/**
 * E2E test: npm/npx version and npm init through kernel.
 *
 * Verifies:
 *   - npm --version outputs valid semver
 *   - npx --version outputs valid semver
 *   - npm init -y creates package.json with default values
 *
 * These are offline tests (no network required).
 *
 * Note: kernel.exec() wraps commands in sh -c; brush-shell returns exit
 * code 17 for spawned children. Test stdout content, not exit code.
 */

import { describe, expect, it } from 'vitest';
import { describeIf, createIntegrationKernel, skipUnlessWasmBuilt } from './helpers.ts';

const skipReason = skipUnlessWasmBuilt();

describeIf(!skipReason, 'e2e npm/npx version and init', () => {
  it('npm --version returns valid semver', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      const result = await kernel.exec('npm --version', { cwd: '/' });
      const version = result.stdout.trim();
      // Valid semver: major.minor.patch (optionally with pre-release)
      expect(version).toMatch(/\d+\.\d+\.\d+/);
    } finally {
      await dispose();
    }
  }, 30_000);

  it('npx --version returns valid semver', async () => {
    const { kernel, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      const result = await kernel.exec('npx --version', { cwd: '/' });
      const version = result.stdout.trim();
      expect(version).toMatch(/\d+\.\d+\.\d+/);
    } finally {
      await dispose();
    }
  }, 30_000);

  it('npm init -y creates package.json with default values', async () => {
    const { kernel, vfs, dispose } = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
    });

    try {
      await kernel.exec('npm init -y', { cwd: '/' });

      const exists = await vfs.exists('/package.json');
      expect(exists).toBe(true);

      const content = await vfs.readTextFile('/package.json');
      const pkg = JSON.parse(content);
      expect(pkg).toHaveProperty('name');
      expect(pkg).toHaveProperty('version');
    } finally {
      await dispose();
    }
  }, 30_000);
});
