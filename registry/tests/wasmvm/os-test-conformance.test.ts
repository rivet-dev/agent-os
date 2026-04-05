/**
 * POSIX conformance tests — os-test suite
 *
 * Discovers all compiled os-test WASM binaries, checks them against the
 * exclusion list, and runs everything not excluded through the WasmVM kernel.
 * Native binaries are run for parity comparison where available.
 *
 * Tests skip gracefully when WASM binaries are not built.
 */

import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os/test/runtime';
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
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';

interface ExclusionEntry {
  expected: 'fail' | 'skip';
  reason: string;
  category: string;
  issue?: string;
}
import exclusionsData from './os-test-exclusions.json' with { type: 'json' };

// ── Paths ──────────────────────────────────────────────────────────────

const C_SRC_DIR = resolve(C_BUILD_DIR, '..');
const OS_TEST_WASM_DIR = join(C_BUILD_DIR, 'os-test');
const OS_TEST_NATIVE_DIR = join(C_BUILD_DIR, 'native', 'os-test');
const OS_TEST_SRC_DIR = join(C_SRC_DIR, 'os-test');
const REPORT_PATH = join(C_BUILD_DIR, '..', '..', 'os-test-conformance-report.json');

const TEST_TIMEOUT_MS = 30_000;
const NATIVE_TIMEOUT_MS = 25_000;

const hasOsTestWasm = existsSync(OS_TEST_WASM_DIR);

// ── Skip guard ─────────────────────────────────────────────────────────

function skipReason(): string | false {
  if (!hasWasmBinaries) return 'WASM runtime binaries not built (run make wasm in native/wasmvm/)';
  if (!hasOsTestWasm) return 'os-test WASM binaries not built (run make -C native/wasmvm/c os-test)';
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

// ── VFS population from native build ──────────────────────────────────
// Mirror the native build directory structure into the in-memory VFS
// so that os-test binaries that use opendir/readdir/scandir/nftw see the
// expected directory layout at the VFS root.
//
// Entries are created at TWO levels:
//   1. Root level (/<subdir>/<test>) — tests using relative paths from cwd /
//   2. Suite level (/<suite>/<subdir>/<test>) — tests that navigate via ".."
//      and reference entries by suite-qualified path (e.g., fstatat opens
//      ".." then stats "basic/sys_stat/fstatat")

async function populateVfsForSuite(
  fs: ReturnType<typeof createInMemoryFileSystem>,
  suite: string,
): Promise<void> {
  const suiteNativeDir = join(OS_TEST_NATIVE_DIR, suite);
  if (!existsSync(suiteNativeDir)) return;

  function collect(dir: string, prefix: string): { dirs: string[]; files: { vfsPath: string; hostPath: string }[] } {
    const result: { dirs: string[]; files: { vfsPath: string; hostPath: string }[] } = { dirs: [], files: [] };
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        result.dirs.push(`/${rel}`);
        const sub = collect(join(dir, entry.name), rel);
        result.dirs.push(...sub.dirs);
        result.files.push(...sub.files);
      } else {
        result.files.push({ vfsPath: `/${rel}`, hostPath: join(dir, entry.name) });
      }
    }
    return result;
  }

  /** Write a VFS file with non-zero content matching the host file size.
   *  Tests like lseek(SEEK_END) and read() need non-zero file sizes. */
  async function writeVfsFile(vfsPath: string, hostPath: string): Promise<void> {
    const size = Math.max(statSync(hostPath).size, 1);
    await fs.writeFile(vfsPath, new Uint8Array(size));
  }

  // Root level — keeps relative-path tests working (e.g., stat("sys_stat/stat"))
  const rootEntries = collect(suiteNativeDir, '');
  for (const d of rootEntries.dirs) {
    await fs.mkdir(d);
  }
  for (const f of rootEntries.files) {
    await writeVfsFile(f.vfsPath, f.hostPath);
  }

  // Suite level — enables parent-relative lookups (e.g., fstatat via "..")
  await fs.mkdir(`/${suite}`);
  const suiteEntries = collect(suiteNativeDir, suite);
  for (const d of suiteEntries.dirs) {
    await fs.mkdir(d);
  }
  for (const f of suiteEntries.files) {
    await writeVfsFile(f.vfsPath, f.hostPath);
  }

  // Source tree — provides .c files for faccessat-style tests that check
  // source file existence (e.g., faccessat(dir, "basic/unistd/faccessat.c"))
  const suiteSrcDir = join(OS_TEST_SRC_DIR, suite);
  if (existsSync(suiteSrcDir)) {
    const srcEntries = collect(suiteSrcDir, suite);
    for (const d of srcEntries.dirs) {
      try { await fs.mkdir(d); } catch { /* already exists from native entries */ }
    }
    for (const f of srcEntries.files) {
      try { await writeVfsFile(f.vfsPath, f.hostPath); } catch { /* already exists */ }
    }
  }
}

// ── Flat symlink directory for command resolution ──────────────────────
// The kernel resolves commands by basename — nested paths like
// "basic/arpa_inet/htonl" lose their directory context. We create a flat
// temp directory with uniquely-named symlinks so every os-test binary
// is addressable as a single command name (e.g., "basic--arpa_inet--htonl").

function toFlatName(testName: string): string {
  return testName.replaceAll('/', '--');
}

const allTests = hasOsTestWasm ? discoverTests(OS_TEST_WASM_DIR) : [];
let FLAT_CMD_DIR: string | undefined;

if (allTests.length > 0) {
  FLAT_CMD_DIR = mkdtempSync(join(tmpdir(), 'os-test-'));
  for (const test of allTests) {
    symlinkSync(join(OS_TEST_WASM_DIR, test), join(FLAT_CMD_DIR, toFlatName(test)));
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
  wasmStderr?: string;
  nativeStderr?: string;
  error?: string;
}

const testResults: TestResult[] = [];

// ── Report generation ──────────────────────────────────────────────────

function writeConformanceReport(results: TestResult[]): void {
  // Per-suite breakdown
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

  // Count passing tests that had a native binary available for output comparison.
  // These tests were verified to produce the same stdout as the native binary.
  const nativeVerifiedCount = results.filter(
    (r) => r.status === 'pass' && r.nativeExitCode !== undefined,
  ).length;

  const report = {
    osTestVersion: exclusionsData.osTestVersion,
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
  console.log(`POSIX Conformance Summary (os-test v${exclusionsData.osTestVersion})`);
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
  const stderrWarnings = results.filter(
    (r) => r.status === 'pass' && r.wasmStderr,
  ).length;

  console.log(`Expected fail: ${fail}`);
  console.log(`Must-pass:     ${mustPass - fail} (${pass} passing)`);
  if (stderrWarnings > 0) {
    console.log(`Stderr warns:  ${stderrWarnings} passing tests have unexpected stderr`);
  }
  console.log('');
}

// ── Test suite ─────────────────────────────────────────────────────────

describe.skipIf(skipReason())('POSIX conformance (os-test)', () => {
  afterAll(() => {
    if (testResults.length > 0) {
      writeConformanceReport(testResults);
      printSummary(testResults);
    }
    // Clean up temp symlink directory
    if (FLAT_CMD_DIR) {
      try { rmSync(FLAT_CMD_DIR, { recursive: true, force: true }); } catch { /* ignore */ }
    }
  });

  for (const [suite, tests] of bySuite) {
    describe(`posix/${suite}`, () => {
      let kernel: Kernel;

      // Native cwd: run from the suite's native build directory so tests
      // that use opendir/readdir/nftw find sibling directories (e.g.,
      // basic/dirent/readdir expects to find "dirent" in cwd when run
      // from basic/).
      const nativeSuiteCwd = join(OS_TEST_NATIVE_DIR, suite);

      beforeAll(async () => {
        // Populate the VFS with directory structure mirroring the native
        // build so WASM binaries see the same entries via opendir/readdir.
        const filesystem = createInMemoryFileSystem();
        if (existsSync(nativeSuiteCwd)) {
          await populateVfsForSuite(filesystem, suite);
        }
        kernel = createKernel({ filesystem, cwd: '/' });
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

          // Run natively (if binary exists) from suite build directory
          const nativePath = join(OS_TEST_NATIVE_DIR, testName);
          const nativeResult = existsSync(nativePath)
            ? await runNative(nativePath, existsSync(nativeSuiteCwd) ? nativeSuiteCwd : undefined)
            : null;

          // Run in WASM via kernel.spawn() (bypasses sh -c wrapper to get real exit code)
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

          // Warn on unexpected stderr for passing WASM tests
          const hasUnexpectedStderr = wasmResult.exitCode === 0
            && wasmStderr.trim().length > 0
            && nativeResult && nativeResult.stderr.trim().length === 0;

          if (exclusion?.expected === 'fail') {
            // Known failure — must still fail (exit non-0 OR parity mismatch)
            const exitOk = wasmResult.exitCode === 0;
            const parityOk = !nativeResult || nativeResult.exitCode !== 0 ||
              wasmResult.stdout.trim() === nativeResult.stdout.trim();
            // Native parity: both fail identically (same exit code + stdout)
            const nativeParityPass = !!nativeResult &&
              nativeResult.exitCode !== 0 &&
              wasmResult.exitCode === nativeResult.exitCode &&
              wasmResult.stdout.trim() === nativeResult.stdout.trim();

            if ((exitOk && parityOk) || nativeParityPass) {
              testResults.push({
                name: testName, suite, status: 'pass',
                wasmExitCode: 0, nativeExitCode: nativeResult?.exitCode,
                wasmStderr: wasmStderr || undefined,
                nativeStderr: nativeResult?.stderr || undefined,
              });
              throw new Error(
                `${testName} is excluded as "fail" but now passes! ` +
                'Remove it from os-test-exclusions.json to lock in this fix.',
              );
            }
            testResults.push({
              name: testName, suite, status: 'fail',
              wasmExitCode: wasmResult.exitCode,
              nativeExitCode: nativeResult?.exitCode,
              wasmStderr: wasmStderr || undefined,
              nativeStderr: nativeResult?.stderr || undefined,
            });
          } else {
            // Not excluded — must pass (or match native failure exactly)
            try {
              // Native parity: if native also fails with the same exit code
              // and output, WASM matching that failure IS correct behavior
              // (e.g., Sortix-specific paths like /dev/ptc that don't exist
              // on real Linux either).
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

                // Native parity check: if native passes, compare output
                if (nativeResult && nativeResult.exitCode === 0) {
                  expect(wasmResult.stdout.trim()).toBe(nativeResult.stdout.trim());
                }

                testResults.push({
                  name: testName, suite, status: 'pass',
                  wasmExitCode: wasmResult.exitCode,
                  nativeExitCode: nativeResult?.exitCode,
                  wasmStderr: hasUnexpectedStderr ? wasmStderr : undefined,
                  nativeStderr: hasUnexpectedStderr ? nativeResult?.stderr : undefined,
                });

                if (hasUnexpectedStderr) {
                  console.warn(
                    `⚠ ${testName}: passes but has unexpected stderr in WASM:\n  ${wasmStderr.trim().split('\n').join('\n  ')}`,
                  );
                }
              }
            } catch (err) {
              testResults.push({
                name: testName, suite, status: 'fail',
                wasmExitCode: wasmResult.exitCode,
                nativeExitCode: nativeResult?.exitCode,
                wasmStderr: wasmStderr || undefined,
                nativeStderr: nativeResult?.stderr || undefined,
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
