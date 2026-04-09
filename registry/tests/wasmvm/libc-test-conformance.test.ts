/**
 * Kernel behavior conformance tests — musl libc-test suite
 *
 * Discovers all compiled libc-test WASM binaries (functional/ and regression/),
 * checks them against the exclusion list, and runs everything not excluded
 * through the WasmVM kernel. Native binaries are run for parity comparison.
 *
 * Unlike os-test (which tests libc function correctness), libc-test exercises
 * kernel-level behavior: file locking, socket operations, stat edge cases,
 * signal delivery, and process management.
 *
 * Tests skip gracefully when WASM binaries are not built.
 */

import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import {
  COMMANDS_DIR,
  C_BUILD_DIR,
  createInMemoryFileSystem,
  createKernel,
  hasWasmBinaries,
} from '../helpers.js';
import type { Kernel } from '../helpers.js';
import {
  existsSync,
  readdirSync,
  statSync,
  symlinkSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { spawn } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
interface ExclusionEntry {
  expected: 'fail' | 'skip';
  reason: string;
  category: string;
  issue?: string;
}
import exclusionsData from './libc-test-exclusions.json' with { type: 'json' };

// ── Paths ──────────────────────────────────────────────────────────────

const LIBC_TEST_WASM_DIR = join(C_BUILD_DIR, 'libc-test');
const LIBC_TEST_NATIVE_DIR = join(C_BUILD_DIR, 'native', 'libc-test');
const REPORT_PATH = join(C_BUILD_DIR, '..', '..', 'libc-test-conformance-report.json');

const TEST_TIMEOUT_MS = 30_000;
const NATIVE_TIMEOUT_MS = 25_000;

const hasLibcTestWasm = existsSync(LIBC_TEST_WASM_DIR);

// ── Skip guard ─────────────────────────────────────────────────────────

function skipReason(): string | false {
  if (!hasWasmBinaries) return 'WASM runtime binaries not built (run make wasm in native/wasmvm/)';
  if (!hasLibcTestWasm) return 'libc-test WASM binaries not built (run make -C native/wasmvm/c libc-test)';
  return false;
}

// ── Test discovery ─────────────────────────────────────────────────────

function discoverTests(dir: string, prefix = ''): string[] {
  if (!existsSync(dir)) return [];
  const results: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      results.push(...discoverTests(join(dir, entry.name), rel));
    } else {
      results.push(rel);
    }
  }
  return results.sort();
}

// ── Native binary runner ───────────────────────────────────────────────

function runNative(
  path: string,
  cwd?: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((res) => {
    const proc = spawn(path, [], { stdio: ['pipe', 'pipe', 'pipe'], cwd });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => {
      proc.kill('SIGKILL');
    }, NATIVE_TIMEOUT_MS);

    proc.stdout.on('data', (d: Buffer) => { stdout += d.toString(); });
    proc.stderr.on('data', (d: Buffer) => { stderr += d.toString(); });
    proc.stdin.end();
    proc.on('close', (code) => {
      clearTimeout(timer);
      res({ exitCode: code ?? 1, stdout, stderr });
    });
  });
}

// ── Flat symlink directory for command resolution ──────────────────────

function toFlatName(testName: string): string {
  return 'libc-test--' + testName.replaceAll('/', '--');
}

const allTests = hasLibcTestWasm ? discoverTests(LIBC_TEST_WASM_DIR) : [];
let FLAT_CMD_DIR: string | undefined;

if (allTests.length > 0) {
  FLAT_CMD_DIR = mkdtempSync(join(tmpdir(), 'libc-test-'));
  for (const test of allTests) {
    symlinkSync(join(LIBC_TEST_WASM_DIR, test), join(FLAT_CMD_DIR, toFlatName(test)));
  }
}

// ── Exclusion map ──────────────────────────────────────────────────────

const exclusions = exclusionsData.exclusions as Record<string, ExclusionEntry>;

// ── Group by suite ─────────────────────────────────────────────────────

const bySuite = new Map<string, string[]>();
for (const test of allTests) {
  const suite = test.includes('/') ? test.split('/')[0] : 'root';
  if (!bySuite.has(suite)) bySuite.set(suite, []);
  bySuite.get(suite)!.push(test);
}

// ── Result tracking ────────────────────────────────────────────────────

interface TestResult {
  name: string;
  suite: string;
  status: 'pass' | 'fail' | 'skip';
  wasmExitCode?: number;
  nativeExitCode?: number;
  wasmStdout?: string;
  nativeStdout?: string;
  wasmStderr?: string;
  nativeStderr?: string;
  error?: string;
}

const testResults: TestResult[] = [];

// ── Report generation ──────────────────────────────────────────────────

function writeConformanceReport(results: TestResult[]): void {
  const suites: Record<string, { total: number; pass: number; fail: number; skip: number }> = {};
  for (const r of results) {
    if (!suites[r.suite]) suites[r.suite] = { total: 0, pass: 0, fail: 0, skip: 0 };
    suites[r.suite].total++;
    suites[r.suite][r.status]++;
  }

  const total = results.length;
  const pass = results.filter((r) => r.status === 'pass').length;
  const fail = results.filter((r) => r.status === 'fail').length;
  const skip = results.filter((r) => r.status === 'skip').length;
  const passRate = total - skip > 0
    ? ((pass / (total - skip)) * 100).toFixed(1)
    : '0.0';

  const nativeVerifiedCount = results.filter(
    (r) => r.status === 'pass' && r.nativeExitCode !== undefined,
  ).length;

  const report = {
    libcTestVersion: exclusionsData.libcTestVersion,
    timestamp: new Date().toISOString(),
    total,
    pass,
    fail,
    skip,
    passRate: `${passRate}%`,
    nativeVerified: nativeVerifiedCount,
    suites,
    tests: results,
  };

  writeFileSync(REPORT_PATH, JSON.stringify(report, null, 2));
}

function printSummary(results: TestResult[]): void {
  const suites: Record<string, { total: number; pass: number; fail: number; skip: number }> = {};
  for (const r of results) {
    if (!suites[r.suite]) suites[r.suite] = { total: 0, pass: 0, fail: 0, skip: 0 };
    suites[r.suite].total++;
    suites[r.suite][r.status]++;
  }

  const total = results.length;
  const pass = results.filter((r) => r.status === 'pass').length;
  const fail = results.filter((r) => r.status === 'fail').length;
  const skip = results.filter((r) => r.status === 'skip').length;
  const mustPass = total - skip;
  const passRate = mustPass > 0 ? ((pass / mustPass) * 100).toFixed(1) : '—';

  console.log('');
  console.log(`libc-test Conformance Summary (musl libc-test ${exclusionsData.libcTestVersion})`);
  console.log('─'.repeat(60));
  console.log(
    'Suite'.padEnd(20) +
    'Total'.padStart(8) +
    'Pass'.padStart(8) +
    'Fail'.padStart(8) +
    'Skip'.padStart(8) +
    'Rate'.padStart(10),
  );

  for (const [name, s] of Object.entries(suites).sort(([a], [b]) => a.localeCompare(b))) {
    const runnable = s.total - s.skip;
    const rate = runnable > 0
      ? ((s.pass / runnable) * 100).toFixed(1) + '%'
      : '—';
    console.log(
      name.padEnd(20) +
      String(s.total).padStart(8) +
      String(s.pass).padStart(8) +
      String(s.fail).padStart(8) +
      String(s.skip).padStart(8) +
      rate.padStart(10),
    );
  }

  console.log('─'.repeat(60));
  console.log(
    'TOTAL'.padEnd(20) +
    String(total).padStart(8) +
    String(pass).padStart(8) +
    String(fail).padStart(8) +
    String(skip).padStart(8) +
    (passRate + (passRate !== '—' ? '%' : '')).padStart(10),
  );

  console.log(`Expected fail: ${fail}`);
  console.log(`Must-pass:     ${mustPass - fail} (${pass} passing)`);
  console.log('');
}

// ── Test suite ─────────────────────────────────────────────────────────

describe.skipIf(skipReason())('libc-test conformance (musl)', () => {
  afterAll(() => {
    if (testResults.length > 0) {
      writeConformanceReport(testResults);
      printSummary(testResults);
    }
    if (FLAT_CMD_DIR) {
      try { rmSync(FLAT_CMD_DIR, { recursive: true, force: true }); } catch { /* ignore */ }
    }
  });

  for (const [suite, tests] of bySuite) {
    describe(`libc-test/${suite}`, () => {
      let kernel: Kernel;

      const nativeSuiteCwd = join(LIBC_TEST_NATIVE_DIR, suite);

      beforeAll(async () => {
        // libc-test functional tests mostly operate on files they create
        // themselves (mkstemp, etc.), so a minimal VFS is sufficient
        const filesystem = createInMemoryFileSystem();
        // Create /tmp for tests that use mkstemp/mkdtemp
        await filesystem.mkdir('/tmp');
        kernel = createKernel({ filesystem, cwd: '/tmp' });
        await kernel.mount(
          createWasmVmRuntime({ commandDirs: [FLAT_CMD_DIR!, COMMANDS_DIR] }),
        );
      });

      afterAll(async () => {
        await kernel?.dispose();
      });

      for (const testName of tests) {
        const exclusion = exclusions[testName];

        if (exclusion?.expected === 'skip') {
          it.skip(`${testName} — ${exclusion.reason}`, () => {});
          testResults.push({ name: testName, suite, status: 'skip' });
          continue;
        }

        it(testName, async () => {
          const flatName = toFlatName(testName);

          // Run natively (if binary exists)
          const nativePath = join(LIBC_TEST_NATIVE_DIR, testName);
          const nativeResult = existsSync(nativePath)
            ? await runNative(nativePath, existsSync(nativeSuiteCwd) ? nativeSuiteCwd : undefined)
            : null;

          // Run in WASM via kernel.spawn()
          const stdoutChunks: Uint8Array[] = [];
          const stderrChunks: Uint8Array[] = [];
          const proc = kernel.spawn(flatName, [], {
            onStdout: (d) => stdoutChunks.push(d),
            onStderr: (d) => stderrChunks.push(d),
            timeout: NATIVE_TIMEOUT_MS,
          });
          proc.closeStdin();
          const wasmExitCode = await proc.wait();
          const wasmStdout = Buffer.concat(stdoutChunks).toString();
          const wasmStderr = Buffer.concat(stderrChunks).toString();
          const wasmResult = { exitCode: wasmExitCode, stdout: wasmStdout, stderr: wasmStderr };

          if (exclusion?.expected === 'fail') {
            // Known failure — must still fail
            const exitOk = wasmResult.exitCode === 0;
            const parityOk = !nativeResult || nativeResult.exitCode !== 0 ||
              wasmResult.stdout.trim() === nativeResult.stdout.trim();
            const nativeParityPass = !!nativeResult &&
              nativeResult.exitCode !== 0 &&
              wasmResult.exitCode === nativeResult.exitCode &&
              wasmResult.stdout.trim() === nativeResult.stdout.trim();

            if ((exitOk && parityOk) || nativeParityPass) {
              testResults.push({
                name: testName, suite, status: 'pass',
                wasmExitCode: 0, nativeExitCode: nativeResult?.exitCode,
                wasmStdout: wasmStdout || undefined,
                wasmStderr: wasmStderr || undefined,
              });
              throw new Error(
                `${testName} is excluded as "fail" but now passes! ` +
                'Remove it from libc-test-exclusions.json to lock in this fix.',
              );
            }
            testResults.push({
              name: testName, suite, status: 'fail',
              wasmExitCode: wasmResult.exitCode,
              nativeExitCode: nativeResult?.exitCode,
              wasmStdout: wasmStdout || undefined,
              wasmStderr: wasmStderr || undefined,
            });
          } else {
            // Not excluded — must pass (or match native failure exactly)
            try {
              // libc-test pass = exit 0, no stdout output
              // libc-test fail = exit 1, error messages on stdout
              if (nativeResult && nativeResult.exitCode !== 0 &&
                  wasmResult.exitCode === nativeResult.exitCode &&
                  wasmResult.stdout.trim() === nativeResult.stdout.trim()) {
                // Both fail identically — native parity
                testResults.push({
                  name: testName, suite, status: 'pass',
                  wasmExitCode: wasmResult.exitCode,
                  nativeExitCode: nativeResult.exitCode,
                });
              } else {
                expect(wasmResult.exitCode).toBe(0);

                // Native parity: if native passes, output should match
                // (libc-test passes produce no stdout)
                if (nativeResult && nativeResult.exitCode === 0) {
                  expect(wasmResult.stdout.trim()).toBe(nativeResult.stdout.trim());
                }

                testResults.push({
                  name: testName, suite, status: 'pass',
                  wasmExitCode: wasmResult.exitCode,
                  nativeExitCode: nativeResult?.exitCode,
                });
              }
            } catch (err) {
              testResults.push({
                name: testName, suite, status: 'fail',
                wasmExitCode: wasmResult.exitCode,
                nativeExitCode: nativeResult?.exitCode,
                wasmStdout: wasmStdout || undefined,
                wasmStderr: wasmStderr || undefined,
                error: (err as Error).message,
              });
              throw err;
            }
          }
        }, TEST_TIMEOUT_MS);
      }
    });
  }
});
