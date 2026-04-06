/**
 * E2E project-matrix test: run existing fixture projects through the kernel.
 *
 * For each fixture in the repo-owned tests/projects/ directory:
 *   1. Prepare project (npm install, cached by content hash)
 *   2. Run entry via host Node (baseline)
 *   3. Run entry via kernel (NodeFileSystem rooted at project dir, WasmVM + Node)
 *   4. Compare output parity
 *
 * Adapted from the legacy runtime suite to use package imports and
 * repo-local fixtures.
 */

import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { access, cp, mkdir, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { describe, expect, it } from 'vitest';
import {
  COMMANDS_DIR,
  createKernel,
  NodeFileSystem,
  createWasmVmRuntime,
  createNodeRuntime,
  skipUnlessWasmBuilt,
} from '../helpers.ts';

const execFileAsync = promisify(execFile);
const TEST_TIMEOUT_MS = 55_000;
const COMMAND_TIMEOUT_MS = 45_000;
const CACHE_READY_MARKER = '.ready';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const WORKSPACE_ROOT = path.resolve(__dirname, '../../../..');
const FIXTURES_ROOT = path.resolve(__dirname, '../../projects');
const CACHE_ROOT = path.join(__dirname, '../../../.cache', 'project-matrix');

// ---------------------------------------------------------------------------
// Types (same schema as project-matrix.test.ts)
// ---------------------------------------------------------------------------

type PackageManager = 'pnpm' | 'npm' | 'bun' | 'yarn';
type PassFixtureMetadata = { entry: string; expectation: 'pass'; packageManager?: PackageManager };
type FailFixtureMetadata = {
  entry: string;
  expectation: 'fail';
  fail: { code: number; stderrIncludes: string };
  packageManager?: PackageManager;
};
type FixtureMetadata = PassFixtureMetadata | FailFixtureMetadata;
type FixtureProject = { name: string; sourceDir: string; metadata: FixtureMetadata };
type PreparedFixture = { cacheHit: boolean; cacheKey: string; projectDir: string };
type ResultEnvelope = { code: number; stdout: string; stderr: string };

// ---------------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------------

async function discoverFixtures(): Promise<FixtureProject[]> {
  let entries;
  try {
    entries = await readdir(FIXTURES_ROOT, { withFileTypes: true });
  } catch {
    // Fixtures directory doesn't exist in registry. Return empty.
    return [];
  }
  const fixtureDirs = entries
    .filter((e) => e.isDirectory())
    .map((e) => e.name)
    .sort((a, b) => a.localeCompare(b));

  const fixtures: FixtureProject[] = [];
  for (const name of fixtureDirs) {
    const sourceDir = path.join(FIXTURES_ROOT, name);
    const metaPath = path.join(sourceDir, 'fixture.json');
    const packageJsonPath = path.join(sourceDir, 'package.json');
    if (!(await pathExists(metaPath)) || !(await pathExists(packageJsonPath))) {
      continue;
    }
    const raw = JSON.parse(await readFile(metaPath, 'utf8'));
    const metadata = parseMetadata(raw, name);
    fixtures.push({ name, sourceDir, metadata });
  }
  return fixtures;
}

function parseMetadata(raw: Record<string, unknown>, name: string): FixtureMetadata {
  const entry = raw.entry as string;
  const packageManager = raw.packageManager as PackageManager | undefined;
  if (raw.expectation === 'pass') return { entry, expectation: 'pass', ...(packageManager && { packageManager }) };
  const fail = raw.fail as { code: number; stderrIncludes: string };
  return { entry, expectation: 'fail', fail, ...(packageManager && { packageManager }) };
}

// ---------------------------------------------------------------------------
// Fixture preparation
// ---------------------------------------------------------------------------

async function prepareFixtureProject(fixture: FixtureProject): Promise<PreparedFixture> {
  await mkdir(CACHE_ROOT, { recursive: true });
  const cacheKey = await createFixtureCacheKey(fixture);
  const cacheDir = path.join(CACHE_ROOT, `${fixture.name}-${cacheKey}`);
  const readyMarker = path.join(cacheDir, CACHE_READY_MARKER);

  if (await pathExists(readyMarker)) {
    return { cacheHit: true, cacheKey, projectDir: cacheDir };
  }

  // Reset stale entries
  if (await pathExists(cacheDir)) {
    await rm(cacheDir, { recursive: true, force: true });
  }

  // Stage and install
  const staging = `${cacheDir}.tmp-${process.pid}-${Date.now()}`;
  await rm(staging, { recursive: true, force: true });
  await cp(fixture.sourceDir, staging, {
    recursive: true,
    filter: (src) => !src.split(path.sep).includes('node_modules'),
  });
  const pm = fixture.metadata.packageManager ?? 'pnpm';
  const installCmd =
    pm === 'npm'
      ? { cmd: 'npm', args: ['install', '--prefer-offline'] }
      : pm === 'bun'
        ? { cmd: 'bun', args: ['install'] }
        : pm === 'yarn'
          ? await getYarnInstallCmd(staging)
          : { cmd: 'pnpm', args: ['install', '--ignore-workspace', '--prefer-offline'] };
  await execFileAsync(installCmd.cmd, installCmd.args, {
    cwd: staging,
    timeout: COMMAND_TIMEOUT_MS,
    maxBuffer: 10 * 1024 * 1024,
    ...(pm === 'yarn' && { env: yarnEnv }),
  });
  await writeFile(path.join(staging, CACHE_READY_MARKER), `${new Date().toISOString()}\n`);

  // Promote
  try {
    await rename(staging, cacheDir);
  } catch (err: unknown) {
    const code = err && typeof err === 'object' && 'code' in err ? String(err.code) : '';
    if (code !== 'EEXIST') throw err;
    await rm(staging, { recursive: true, force: true });
    if (!(await pathExists(readyMarker))) {
      throw new Error(`Cache race: missing ready marker at ${cacheDir}`);
    }
  }

  return { cacheHit: false, cacheKey, projectDir: cacheDir };
}

async function createFixtureCacheKey(fixture: FixtureProject): Promise<string> {
  const hash = createHash('sha256');
  const nodeMajor = process.versions.node.split('.')[0] ?? '0';
  const pm = fixture.metadata.packageManager ?? 'pnpm';
  const pmVersion =
    pm === 'npm'
      ? await getNpmVersion()
      : pm === 'bun'
        ? await getBunVersion()
        : pm === 'yarn'
          ? await getYarnVersion()
          : await getPnpmVersion();
  hash.update(`node-major:${nodeMajor}\n`);
  hash.update(`pm:${pm}\n`);
  hash.update(`pm-version:${pmVersion}\n`);
  hash.update(`platform:${process.platform}\n`);
  hash.update(`arch:${process.arch}\n`);

  const lockFile =
    pm === 'npm'
      ? 'package-lock.json'
      : pm === 'bun'
        ? 'bun.lock'
        : pm === 'yarn'
          ? 'yarn.lock'
          : 'pnpm-lock.yaml';
  for (const [label, filePath] of [
    ['workspace-lock', path.join(WORKSPACE_ROOT, 'pnpm-lock.yaml')],
    ['workspace-package', path.join(WORKSPACE_ROOT, 'package.json')],
    ['fixture-package', path.join(fixture.sourceDir, 'package.json')],
    ['fixture-lock', path.join(fixture.sourceDir, lockFile)],
  ]) {
    hash.update(`${label}:`);
    try { hash.update(await readFile(filePath)); } catch { hash.update('<missing>'); }
    hash.update('\n');
  }

  const files = await listFiles(fixture.sourceDir);
  for (const rel of files) {
    hash.update(`fixture-file:${rel.split(path.sep).join('/')}\n`);
    hash.update(await readFile(path.join(fixture.sourceDir, rel)));
    hash.update('\n');
  }

  return hash.digest('hex').slice(0, 16);
}

let _pnpmVersionPromise: Promise<string> | undefined;
function getPnpmVersion(): Promise<string> {
  if (!_pnpmVersionPromise) {
    _pnpmVersionPromise = execFileAsync('pnpm', ['--version'], {
      cwd: WORKSPACE_ROOT,
      timeout: COMMAND_TIMEOUT_MS,
    }).then((r) => r.stdout.trim());
  }
  return _pnpmVersionPromise;
}

let _npmVersionPromise: Promise<string> | undefined;
function getNpmVersion(): Promise<string> {
  if (!_npmVersionPromise) {
    _npmVersionPromise = execFileAsync('npm', ['--version'], {
      cwd: WORKSPACE_ROOT,
      timeout: COMMAND_TIMEOUT_MS,
    }).then((r) => r.stdout.trim());
  }
  return _npmVersionPromise;
}

let _bunVersionPromise: Promise<string> | undefined;
function getBunVersion(): Promise<string> {
  if (!_bunVersionPromise) {
    _bunVersionPromise = execFileAsync('bun', ['--version'], {
      cwd: WORKSPACE_ROOT,
      timeout: COMMAND_TIMEOUT_MS,
    }).then((r) => r.stdout.trim());
  }
  return _bunVersionPromise;
}

let _yarnVersionPromise: Promise<string> | undefined;
// Bypass corepack packageManager enforcement so yarn runs in a pnpm workspace.
const yarnEnv = { ...process.env, COREPACK_ENABLE_STRICT: '0' };
function getYarnVersion(): Promise<string> {
  if (!_yarnVersionPromise) {
    _yarnVersionPromise = execFileAsync('yarn', ['--version'], {
      cwd: WORKSPACE_ROOT,
      timeout: COMMAND_TIMEOUT_MS,
      env: yarnEnv,
    }).then((r) => r.stdout.trim());
  }
  return _yarnVersionPromise;
}

async function getYarnInstallCmd(
  projectDir: string,
): Promise<{ cmd: string; args: string[] }> {
  const isBerry = await pathExists(path.join(projectDir, '.yarnrc.yml'));
  return isBerry
    ? { cmd: 'yarn', args: ['install', '--immutable'] }
    : { cmd: 'yarn', args: ['install'] };
}

async function listFiles(root: string): Promise<string[]> {
  const result: string[] = [];
  async function walk(rel: string): Promise<void> {
    const dir = path.join(root, rel);
    const entries = await readdir(dir, { withFileTypes: true });
    for (const e of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      if (e.name === 'node_modules') continue;
      const p = rel ? path.join(rel, e.name) : e.name;
      if (e.isDirectory()) await walk(p);
      else if (e.isFile()) result.push(p);
    }
  }
  await walk('');
  return result.sort((a, b) => a.localeCompare(b));
}

// ---------------------------------------------------------------------------
// Host execution (baseline)
// ---------------------------------------------------------------------------

async function runHostExecution(projectDir: string, entryRel: string): Promise<ResultEnvelope> {
  const entryPath = path.join(projectDir, entryRel);
  return normalizeEnvelope(await runCommand(process.execPath, [entryPath], projectDir), projectDir);
}

async function runCommand(cmd: string, args: string[], cwd: string): Promise<ResultEnvelope> {
  try {
    const r = await execFileAsync(cmd, args, { cwd, timeout: COMMAND_TIMEOUT_MS, maxBuffer: 10 * 1024 * 1024 });
    return { code: 0, stdout: r.stdout, stderr: r.stderr };
  } catch (err: unknown) {
    if (err && typeof err === 'object' && 'stdout' in err) {
      const e = err as { code?: number; stdout?: string; stderr?: string };
      return {
        code: typeof e.code === 'number' ? e.code : 1,
        stdout: typeof e.stdout === 'string' ? e.stdout : '',
        stderr: typeof e.stderr === 'string' ? e.stderr : '',
      };
    }
    throw err;
  }
}

// ---------------------------------------------------------------------------
// Kernel execution
// ---------------------------------------------------------------------------

async function runKernelExecution(projectDir: string, entryRel: string): Promise<ResultEnvelope> {
  // NodeFileSystem rooted at projectDir. require() resolves from node_modules on disk.
  const vfs = new NodeFileSystem({ root: projectDir });
  const kernel = createKernel({ filesystem: vfs, cwd: '/' });

  await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
  await kernel.mount(createNodeRuntime());

  try {
    const vfsEntry = '/' + entryRel.replace(/\\/g, '/');
    const result = await kernel.exec(`node ${vfsEntry}`, { cwd: '/' });
    return normalizeEnvelope(
      { code: result.exitCode, stdout: result.stdout, stderr: result.stderr },
      projectDir,
    );
  } finally {
    await kernel.dispose();
  }
}

// ---------------------------------------------------------------------------
// Output normalization
// ---------------------------------------------------------------------------

function normalizeEnvelope(envelope: ResultEnvelope, projectDir: string): ResultEnvelope {
  return {
    code: envelope.code,
    stdout: normalizeText(envelope.stdout, projectDir),
    stderr: normalizeText(envelope.stderr, projectDir),
  };
}

function normalizeText(value: string, projectDir: string): string {
  const normalized = value.replace(/\r\n/g, '\n');
  const posixDir = projectDir.split(path.sep).join(path.posix.sep);
  return normalizeModuleNotFoundText(
    normalized.split(projectDir).join('<project>').split(posixDir).join('<project>'),
  );
}

function normalizeModuleNotFoundText(value: string): string {
  if (!value.includes('Cannot find module')) return value;
  const quoted = value.match(/Cannot find module '([^']+)'/);
  if (quoted) return `Cannot find module '${quoted[1]}'\n`;
  const from = value.match(/Cannot find module:\s*([^\s]+)\s+from\s+/);
  if (from) return `Cannot find module '${from[1]}'\n`;
  return value;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function pathExists(p: string): Promise<boolean> {
  try { await access(p); return true; } catch { return false; }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const skipReason = skipUnlessWasmBuilt();
const discoveredFixtures = await discoverFixtures();

describe.skipIf(skipReason || discoveredFixtures.length === 0)('e2e project-matrix through kernel', () => {
  it('discovers at least one fixture project', () => {
    expect(discoveredFixtures.length).toBeGreaterThan(0);
  });

  for (const fixture of discoveredFixtures) {
    it(
      `runs fixture ${fixture.name} through kernel with host-node parity`,
      async () => {
        const prepared = await prepareFixtureProject(fixture);
        const host = await runHostExecution(prepared.projectDir, fixture.metadata.entry);
        const kernel = await runKernelExecution(prepared.projectDir, fixture.metadata.entry);

        if (fixture.metadata.expectation === 'pass') {
          expect(kernel.code).toBe(host.code);
          expect(kernel.stdout).toBe(host.stdout);
          expect(kernel.stderr).toBe(host.stderr);
          return;
        }

        // Fail fixtures: host succeeds, kernel enforces sandbox restrictions
        expect(host.code).toBe(0);
        expect(kernel.code).toBe(fixture.metadata.fail.code);
        expect(kernel.stderr).toContain(fixture.metadata.fail.stderrIncludes);
      },
      TEST_TIMEOUT_MS,
    );
  }
});
