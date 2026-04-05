/**
 * C parity tests — native vs WASM
 *
 * Compiles C test fixtures to both native and WASM, runs both, and
 * compares stdout/stderr/exit code for parity. Tests skip when
 * WASM binaries (make wasm), C WASM binaries (make -C native/wasmvm/c programs),
 * or native binaries (make -C native/wasmvm/c native) are not built.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os/test/runtime';
import { COMMANDS_DIR, C_BUILD_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { existsSync } from 'node:fs';
import { writeFile as fsWriteFile, readFile as fsReadFile, mkdtemp, rm, mkdir as fsMkdir } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer as createTcpServer } from 'node:net';
import { createServer as createHttpServer } from 'node:http';

const NATIVE_DIR = join(C_BUILD_DIR, 'native');

const hasCWasmBinaries = existsSync(join(C_BUILD_DIR, 'hello'));
const hasNativeBinaries = existsSync(join(NATIVE_DIR, 'hello'));

function skipReason(): string | false {
  if (!hasWasmBinaries) return 'WASM binaries not built (run make wasm in native/wasmvm/)';
  if (!hasCWasmBinaries) return 'C WASM binaries not built (run make -C native/wasmvm/c programs)';
  if (!hasNativeBinaries) return 'C native binaries not built (run make -C native/wasmvm/c native)';
  return false;
}

// Run a native binary, capture stdout/stderr/exitCode
function runNative(
  name: string,
  args: string[] = [],
  options?: { input?: string; env?: Record<string, string> },
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((res) => {
    const proc = spawn(join(NATIVE_DIR, name), args, {
      env: options?.env,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';

    proc.stdout.on('data', (d: Buffer) => { stdout += d.toString(); });
    proc.stderr.on('data', (d: Buffer) => { stderr += d.toString(); });

    if (options?.input !== undefined) {
      proc.stdin.write(options.input);
    }
    proc.stdin.end();

    proc.on('close', (code) => {
      res({ exitCode: code ?? 0, stdout, stderr });
    });
  });
}

// Strip kernel-level diagnostic WARN lines from WASM stderr (not program output)
function normalizeStderr(stderr: string): string {
  return stderr
    .split('\n')
    .filter((l) => !l.includes('WARN') || !l.includes('could not retrieve pid'))
    .join('\n');
}

// Normalize argv[0] line since native path differs from WASM command name
function normalizeArgsOutput(output: string): string {
  return output.replace(/^(argv\[0\]=).+$/m, '$1<program>');
}

// Extract lines matching a prefix from env output
function extractEnvPrefix(output: string, prefix: string): string {
  return output
    .split('\n')
    .filter((l) => l.startsWith(prefix))
    .sort()
    .join('\n');
}

// Minimal in-memory VFS for kernel tests
class SimpleVFS {
  private files = new Map<string, Uint8Array>();
  private dirs = new Set<string>(['/']);
  private symlinks = new Map<string, string>();

  async readFile(path: string): Promise<Uint8Array> {
    const data = this.files.get(path);
    if (!data) throw new Error(`ENOENT: ${path}`);
    return data;
  }
  async readTextFile(path: string): Promise<string> {
    return new TextDecoder().decode(await this.readFile(path));
  }
  async pread(path: string, offset: number, length: number): Promise<Uint8Array> {
    const data = await this.readFile(path);
    return data.slice(offset, offset + length);
  }
  async readDir(path: string): Promise<string[]> {
    const prefix = path === '/' ? '/' : path + '/';
    const entries: string[] = [];
    for (const p of [...this.files.keys(), ...this.dirs]) {
      if (p !== path && p.startsWith(prefix)) {
        const rest = p.slice(prefix.length);
        if (!rest.includes('/')) entries.push(rest);
      }
    }
    return entries;
  }
  async readDirWithTypes(path: string) {
    return (await this.readDir(path)).map((name) => ({
      name,
      isDirectory: this.dirs.has(path === '/' ? `/${name}` : `${path}/${name}`),
    }));
  }
  async writeFile(path: string, content: string | Uint8Array): Promise<void> {
    const data = typeof content === 'string' ? new TextEncoder().encode(content) : content;
    this.files.set(path, new Uint8Array(data));
    const parts = path.split('/').filter(Boolean);
    for (let i = 1; i < parts.length; i++) {
      this.dirs.add('/' + parts.slice(0, i).join('/'));
    }
  }
  async createDir(path: string) { this.dirs.add(path); }
  async mkdir(path: string, _options?: { recursive?: boolean }) { this.dirs.add(path); }
  async exists(path: string): Promise<boolean> {
    return this.files.has(path) || this.dirs.has(path) || this.symlinks.has(path);
  }
  async stat(path: string) {
    const isDir = this.dirs.has(path);
    const isSymlink = this.symlinks.has(path);
    const data = this.files.get(path);
    if (!isDir && !isSymlink && !data) throw new Error(`ENOENT: ${path}`);
    return {
      mode: isSymlink ? 0o120777 : (isDir ? 0o40755 : 0o100644),
      size: data?.length ?? 0,
      isDirectory: isDir,
      isSymbolicLink: isSymlink,
      atimeMs: Date.now(),
      mtimeMs: Date.now(),
      ctimeMs: Date.now(),
      birthtimeMs: Date.now(),
      ino: 0,
      nlink: 1,
      uid: 1000,
      gid: 1000,
    };
  }
  async chmod() {}
  async rename(from: string, to: string) {
    const data = this.files.get(from);
    if (data) { this.files.set(to, data); this.files.delete(from); }
  }
  async unlink(path: string) { this.files.delete(path); this.symlinks.delete(path); }
  async rmdir(path: string) { this.dirs.delete(path); }
  async symlink(target: string, linkPath: string) {
    this.symlinks.set(linkPath, target);
    const parts = linkPath.split('/').filter(Boolean);
    for (let i = 1; i < parts.length; i++) {
      this.dirs.add('/' + parts.slice(0, i).join('/'));
    }
  }
  async readlink(path: string): Promise<string> {
    const target = this.symlinks.get(path);
    if (!target) throw new Error(`EINVAL: ${path}`);
    return target;
  }
}

describe.skipIf(skipReason())('C parity: native vs WASM', { timeout: 30_000 }, () => {
  let kernel: Kernel;
  let vfs: SimpleVFS;

  beforeEach(async () => {
    vfs = new SimpleVFS();
    kernel = createKernel({ filesystem: vfs as any });
    // C build dir first so C programs take precedence over same-named Rust commands
    await kernel.mount(createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }));
  });

  afterEach(async () => {
    await kernel?.dispose();
  });

  // --- Tier 1: basic I/O ---

  it('hello: stdout and exit code match', async () => {
    const native = await runNative('hello');
    const wasm = await kernel.exec('hello');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
  });

  it('args: argc and argv[1..] match', async () => {
    const native = await runNative('args', ['foo', 'bar']);
    const wasm = await kernel.exec('args foo bar');

    expect(wasm.exitCode).toBe(native.exitCode);
    // argv[0] differs (native path vs WASM command name), normalize it
    expect(normalizeArgsOutput(wasm.stdout)).toBe(normalizeArgsOutput(native.stdout));
  });

  it('env: user-specified env vars match', async () => {
    const env = { TEST_PARITY_A: 'hello', TEST_PARITY_B: 'world' };
    const native = await runNative('env', [], { env });
    const wasm = await kernel.exec('env', { env });

    expect(wasm.exitCode).toBe(native.exitCode);
    // Shell may inject extra env vars; compare only the TEST_PARITY_ vars
    expect(extractEnvPrefix(wasm.stdout, 'TEST_PARITY_')).toBe(
      extractEnvPrefix(native.stdout, 'TEST_PARITY_'),
    );
  });

  it('exitcode: exit code matches', async () => {
    const native = await runNative('exitcode', ['42']);
    const wasm = await kernel.exec('exitcode 42');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(42);
  });

  it('cat: stdin passthrough matches', async () => {
    const input = 'hello world\nfoo bar\n';
    const native = await runNative('cat', [], { input });
    const wasm = await kernel.exec('cat', { stdin: input });

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
  });

  // --- Tier 1: data processing ---

  it('wc: word/line/byte counts match', async () => {
    const input = 'hello world\nfoo bar baz\n';
    const native = await runNative('wc', [], { input });
    const wasm = await kernel.exec('wc', { stdin: input });

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
  });

  it('fread: file contents match', async () => {
    const content = 'hello from fread test\n';

    // Native: temp file on disk
    const tmpDir = await mkdtemp(join(tmpdir(), 'c-parity-'));
    const filePath = join(tmpDir, 'test.txt');
    await fsWriteFile(filePath, content);
    const native = await runNative('fread', [filePath]);

    // WASM: file on VFS
    await vfs.writeFile('/tmp/test.txt', content);
    const wasm = await kernel.exec('fread /tmp/test.txt');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);

    await rm(tmpDir, { recursive: true });
  });

  it('fwrite: written content matches', async () => {
    const writeContent = 'test content';

    // Native: write to temp dir
    const tmpDir = await mkdtemp(join(tmpdir(), 'c-parity-'));
    const nativePath = join(tmpDir, 'out.txt');
    const native = await runNative('fwrite', [nativePath, writeContent]);
    const nativeFileContent = await fsReadFile(nativePath, 'utf8');

    // WASM: write to VFS
    const wasm = await kernel.exec(`fwrite /tmp/out.txt "${writeContent}"`);
    const wasmFileContent = await vfs.readTextFile('/tmp/out.txt');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasmFileContent).toBe(nativeFileContent);

    await rm(tmpDir, { recursive: true });
  });

  it('pread_pwrite_access: pread/pwrite/access syscalls match', async () => {
    // Native: uses real /tmp
    const tmpDir = await mkdtemp(join(tmpdir(), 'c-parity-'));
    const nativeEnv = { ...process.env, HOME: tmpDir };
    const native = await runNative('pread_pwrite_access', [], { env: nativeEnv });

    // WASM: uses VFS /tmp
    await vfs.createDir('/tmp');
    const wasm = await kernel.exec('pread_pwrite_access');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('total: 0 failures');

    await rm(tmpDir, { recursive: true });
  });

  it('sort: sorted output matches', async () => {
    const input = 'banana\napple\ncherry\ndate\n';
    const native = await runNative('sort', [], { input });
    const wasm = await kernel.exec('sort', { stdin: input });

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
  });

  it('sha256: hex digest matches', async () => {
    const input = 'hello';
    const native = await runNative('sha256', [], { input });
    const wasm = await kernel.exec('sha256', { stdin: input });

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
  });

  // --- Tier 2: custom imports (patched sysroot) ---

  const hasCTier2Binaries = existsSync(join(C_BUILD_DIR, 'pipe_test'));
  const tier2Skip = !hasCTier2Binaries
    ? 'C Tier 2 WASM binaries not built (need patched sysroot: make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs)'
    : false;

  it.skipIf(tier2Skip)('isatty_test: piped stdin/stdout/stderr all report not-a-tty', async () => {
    const native = await runNative('isatty_test');
    const wasm = await kernel.exec('isatty_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
  });

  it.skipIf(tier2Skip)('getpid_test: PID is valid, not hardcoded 42, and consistent', async () => {
    const native = await runNative('getpid_test');
    const wasm = await kernel.exec('getpid_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // PIDs differ between native and WASM, but both should be valid
    expect(wasm.stdout).toContain('pid_positive=yes');
    expect(wasm.stdout).toContain('pid_not_42=yes');
    expect(wasm.stdout).toContain('pid_consistent=yes');
    expect(native.stdout).toContain('pid_positive=yes');
    expect(native.stdout).toContain('pid_not_42=yes');
    expect(native.stdout).toContain('pid_consistent=yes');
    // Verify actual PID value is > 0
    const wasmPid = parseInt(wasm.stdout.match(/^pid=(\d+)/m)?.[1] ?? '0', 10);
    expect(wasmPid).toBeGreaterThan(0);
    expect(wasmPid).not.toBe(42);
  });

  it.skipIf(tier2Skip)('getppid_test: parent PID is valid and positive', async () => {
    const native = await runNative('getppid_test');
    const wasm = await kernel.exec('getppid_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('ppid_positive=yes');
    expect(native.stdout).toContain('ppid_positive=yes');
  });

  it.skipIf(tier2Skip)('userinfo: uid/gid/euid/egid values are specific', async () => {
    const native = await runNative('userinfo');
    const wasm = await kernel.exec('userinfo');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Verify format for both
    const format = /^uid=\d+\ngid=\d+\neuid=\d+\negid=\d+\n$/;
    expect(wasm.stdout).toMatch(format);
    expect(native.stdout).toMatch(format);
    // WASM kernel returns uid/gid = 1000 (sandbox user)
    expect(wasm.stdout).toContain('uid=1000');
    expect(wasm.stdout).toContain('gid=1000');
    expect(wasm.stdout).toContain('euid=1000');
    expect(wasm.stdout).toContain('egid=1000');
  });

  it.skipIf(tier2Skip)('getpwuid_test: passwd entry fields valid', async () => {
    const native = await runNative('getpwuid_test');
    const wasm = await kernel.exec('getpwuid_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Both should get valid passwd entries
    expect(wasm.stdout).toContain('getpwuid: ok');
    expect(wasm.stdout).toContain('pw_name_nonempty: yes');
    expect(wasm.stdout).toContain('pw_uid_match: yes');
    expect(wasm.stdout).toContain('pw_gid_valid: yes');
    expect(wasm.stdout).toContain('pw_dir_nonempty: yes');
    expect(wasm.stdout).toContain('pw_shell_nonempty: yes');
    expect(native.stdout).toContain('getpwuid: ok');
    expect(native.stdout).toContain('pw_name_nonempty: yes');
    expect(native.stdout).toContain('pw_uid_match: yes');
  });

  it.skipIf(tier2Skip)('pipe_test: write through pipe and read back matches', async () => {
    const native = await runNative('pipe_test');
    const wasm = await kernel.exec('pipe_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
  });

  it.skipIf(tier2Skip)('dup_test: write through duplicated fds matches', async () => {
    const native = await runNative('dup_test');
    const wasm = await kernel.exec('dup_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
  });

  it('sleep_test: nanosleep completes successfully', async () => {
    const native = await runNative('sleep_test', ['50']);
    const wasm = await kernel.exec('sleep_test 50');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Both should report successful sleep with >= 80% of requested time
    expect(wasm.stdout).toContain('requested=50ms');
    expect(wasm.stdout).toContain('ok=yes');
    expect(native.stdout).toContain('requested=50ms');
    expect(native.stdout).toContain('ok=yes');
  });

  // --- Tier 3: process management (patched sysroot) ---

  const hasCTier3Binaries = existsSync(join(C_BUILD_DIR, 'spawn_child'));
  const tier3Skip = !hasCTier3Binaries
    ? 'C Tier 3 WASM binaries not built (need patched sysroot: make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs)'
    : false;

  it.skipIf(tier3Skip)('spawn_child: posix_spawn echo, capture stdout via pipe', async () => {
    const native = await runNative('spawn_child');
    const wasm = await kernel.exec('spawn_child');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('child_stdout: hello');
    expect(wasm.stdout).toContain('child_exit: 0');
  });

  it.skipIf(tier3Skip)('spawn_exit_code: child exits non-zero, verify via waitpid', async () => {
    const native = await runNative('spawn_exit_code');
    const wasm = await kernel.exec('spawn_exit_code');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('child_exit_code: 7');
    expect(wasm.stdout).toContain('match: yes');
  });

  it.skipIf(tier3Skip)('pipeline: echo hello | cat via pipe + posix_spawn', async () => {
    const native = await runNative('pipeline');
    const wasm = await kernel.exec('pipeline');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('pipeline_output: hello');
    expect(wasm.stdout).toContain('echo_exit: 0');
    expect(wasm.stdout).toContain('cat_exit: 0');
  });

  it.skipIf(tier3Skip)('kill_child: spawn sleep, kill SIGTERM, verify terminated', async () => {
    const native = await runNative('kill_child');
    const wasm = await kernel.exec('kill_child');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Both should complete the spawn/kill/wait cycle successfully
    expect(wasm.stdout).toContain('spawned: yes');
    expect(wasm.stdout).toContain('kill: ok');
    expect(wasm.stdout).toContain('terminated: yes');
    // Verify child was killed by signal (WIFSIGNALED)
    expect(wasm.stdout).toContain('signaled=yes');
    expect(native.stdout).toContain('signaled=yes');
    // SIGTERM = 15
    expect(wasm.stdout).toContain('termsig=15');
    expect(native.stdout).toContain('termsig=15');
  });

  it.skipIf(tier3Skip)('signal_tests: SIGKILL, kill exited PID, kill invalid PID', async () => {
    const native = await runNative('signal_tests');
    const wasm = await kernel.exec('signal_tests');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);

    // Test 1: SIGKILL — child killed by signal 9
    expect(wasm.stdout).toContain('test_sigkill: ok');
    expect(native.stdout).toContain('test_sigkill: ok');
    expect(wasm.stdout).toContain('sigkill_signaled=yes');
    expect(wasm.stdout).toContain('sigkill_termsig=9');

    // Test 2: kill exited process — ok with either 0 or -1/ESRCH
    expect(wasm.stdout).toContain('test_kill_exited: ok');
    expect(native.stdout).toContain('test_kill_exited: ok');

    // Test 3: kill invalid PID — returns -1
    expect(wasm.stdout).toContain('test_kill_invalid: ok');
    expect(native.stdout).toContain('test_kill_invalid: ok');
  });

  it.skipIf(tier3Skip)('sigaction_behavior: query, SA_RESETHAND, and SA_RESTART parity', async () => {
    const env = { ...process.env, PATH: `${NATIVE_DIR}:${process.env.PATH ?? ''}` };
    const native = await runNative('sigaction_behavior', [], { env });
    const wasm = await kernel.exec('sigaction_behavior');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('sigaction_query_mask_sigterm=yes');
    expect(wasm.stdout).toContain('sigaction_query_flags=yes');
    expect(wasm.stdout).toContain('sa_resethand_handler_calls=1');
    expect(wasm.stdout).toContain('sa_resethand_reset=yes');
    expect(wasm.stdout).toContain('sa_restart_accept=yes');
    expect(wasm.stdout).toContain('sa_restart_child_exit=0');
  });

  it.skipIf(tier3Skip)('getppid_verify: child getppid matches parent getpid', async () => {
    // Native needs getppid_test on PATH for posix_spawnp
    const native = await runNative('getppid_verify', [], {
      env: { ...process.env, PATH: `${NATIVE_DIR}:${process.env.PATH}` },
    });
    const wasm = await kernel.exec('getppid_verify');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('match=yes');
    expect(native.stdout).toContain('match=yes');
    expect(wasm.stdout).toContain('child_exit=0');
    expect(native.stdout).toContain('child_exit=0');
  });

  it.skipIf(tier3Skip)('waitpid_return: waitpid returns correct child PID', async () => {
    const native = await runNative('waitpid_return');
    const wasm = await kernel.exec('waitpid_return');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // waitpid with specific PID returns that PID
    expect(wasm.stdout).toContain('test1_match: yes');
    expect(wasm.stdout).toContain('test1_exit: 0');
    // wait() (waitpid(-1)) returns actual child PID
    expect(wasm.stdout).toContain('test2_match: yes');
    expect(wasm.stdout).toContain('test2_exit: 0');
    // Return values are positive PIDs
    expect(wasm.stdout).toContain('test3_ret1_positive: yes');
    expect(wasm.stdout).toContain('test3_ret2_positive: yes');
  });

  it.skipIf(tier3Skip)('waitpid_edge: concurrent children and invalid PID', async () => {
    const native = await runNative('waitpid_edge');
    const wasm = await kernel.exec('waitpid_edge');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Test 1: 3 concurrent children with correct exit codes
    expect(wasm.stdout).toContain('test1_c1_exit: 1');
    expect(wasm.stdout).toContain('test1_c2_exit: 2');
    expect(wasm.stdout).toContain('test1_c3_exit: 3');
    expect(wasm.stdout).toContain('test1: ok');
    expect(native.stdout).toContain('test1: ok');
    // Test 2: wait() reaps both children with distinct valid PIDs
    expect(wasm.stdout).toContain('test2_r1_valid: yes');
    expect(wasm.stdout).toContain('test2_r2_valid: yes');
    expect(wasm.stdout).toContain('test2_distinct: yes');
    expect(wasm.stdout).toContain('test2: ok');
    expect(native.stdout).toContain('test2: ok');
    // Test 3: waitpid with never-spawned PID returns -1 with error
    expect(wasm.stdout).toContain('test3_ret: -1');
    expect(wasm.stdout).toContain('test3_failed: yes');
    expect(wasm.stdout).toContain('test3: ok');
    expect(native.stdout).toContain('test3: ok');
  });

  it.skipIf(tier3Skip)('pipe_edge: large write, broken pipe, EOF, close-both', async () => {
    const native = await runNative('pipe_edge');
    const wasm = await kernel.exec('pipe_edge');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));

    // Test 1: large write (128KB > 64KB pipe buffer)
    expect(wasm.stdout).toContain('large_write: ok');
    expect(native.stdout).toContain('large_write: ok');
    expect(wasm.stdout).toContain('large_write_bytes=131072');
    expect(native.stdout).toContain('large_write_bytes=131072');

    // Test 2: broken pipe — write to pipe with closed read end
    expect(wasm.stdout).toContain('broken_pipe: ok');
    expect(native.stdout).toContain('broken_pipe: ok');

    // Test 3: EOF — read from pipe with closed write end
    expect(wasm.stdout).toContain('eof_read: ok');
    expect(native.stdout).toContain('eof_read: ok');
    expect(wasm.stdout).toContain('eof_read_result=0');
    expect(native.stdout).toContain('eof_read_result=0');

    // Test 4: close both ends — no crash or leak
    expect(wasm.stdout).toContain('close_both: ok');
    expect(native.stdout).toContain('close_both: ok');
  });

  // --- Capstone: syscall coverage (all tiers, patched sysroot) ---

  const hasSyscallCoverage = existsSync(join(C_BUILD_DIR, 'syscall_coverage'));
  const syscallCoverageSkip = !hasSyscallCoverage
    ? 'syscall_coverage WASM binary not built (need patched sysroot: make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs)'
    : false;

  it.skipIf(syscallCoverageSkip)('syscall_coverage: all syscall categories pass parity', async () => {
    // Pre-create /tmp in VFS for the program's file operations
    await vfs.createDir('/tmp');

    const env = { TEST_SC: '1', PATH: process.env.PATH ?? '/usr/bin:/bin' };
    const native = await runNative('syscall_coverage', [], { env });

    const wasmEnv = { TEST_SC: '1' };
    const wasm = await kernel.exec('syscall_coverage', { env: wasmEnv });

    // Debug: show WASM output if it fails
    if (wasm.exitCode !== 0) {
      console.log('WASM stdout:', wasm.stdout);
      console.log('WASM stderr:', wasm.stderr);
    }

    // Both should exit 0 (all tests pass)
    expect(native.exitCode).toBe(0);
    expect(wasm.exitCode).toBe(0);

    // Compare structured output — normalize host_user lines whose values
    // differ between native (real OS uid) and WASM (always 1000)
    const normalizeSyscallCoverage = (out: string) =>
      out.replace(/^(getuid|getgid|geteuid|getegid): ok$/gm, '$1: ok');
    expect(normalizeSyscallCoverage(wasm.stdout)).toBe(normalizeSyscallCoverage(native.stdout));
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));

    // Verify all expected syscalls are tested
    const expectedSyscalls = [
      // WASI FD ops
      'open', 'write', 'read', 'seek', 'pread', 'pwrite', 'fstat', 'ftruncate', 'close',
      // WASI path ops
      'mkdir', 'stat', 'rename', 'opendir', 'readdir', 'closedir',
      'symlink', 'readlink', 'unlink', 'rmdir',
      // Args/env/clock
      'argc', 'argv', 'environ', 'clock_realtime', 'clock_monotonic',
      // host_process
      'pipe', 'dup', 'dup2', 'getpid', 'getppid', 'sigaction_register', 'sigaction_query', 'spawn_waitpid', 'kill',
      // host_user
      'getuid', 'getgid', 'geteuid', 'getegid', 'isatty_stdin', 'getpwuid',
      // host_net
      'getsockname', 'getpeername',
    ];
    for (const name of expectedSyscalls) {
      expect(wasm.stdout).toContain(`${name}: ok`);
    }
    expect(wasm.stdout).toContain('total: 0 failures');
  });

  // --- Tier 4: filesystem stress ---

  const hasCTier4Binaries = existsSync(join(C_BUILD_DIR, 'c-ls'));
  const hasCTier4Native = existsSync(join(NATIVE_DIR, 'c-ls'));
  const tier4Skip = (!hasCTier4Binaries || !hasCTier4Native)
    ? 'C Tier 4 binaries not built (run make -C native/wasmvm/c programs && make -C native/wasmvm/c native)'
    : false;

  // Helper: create test directory tree on disk and in VFS
  async function setupTestTree(testVfs: SimpleVFS) {
    const tmpDir = await mkdtemp(join(tmpdir(), 'c-parity-tree-'));
    await fsMkdir(join(tmpDir, 'subdir', 'deep'), { recursive: true });
    await fsWriteFile(join(tmpDir, 'alpha.txt'), 'hello\n');
    await fsWriteFile(join(tmpDir, 'beta.txt'), 'world!\n');
    await fsWriteFile(join(tmpDir, 'subdir', 'gamma.txt'), 'nested file\n');
    await fsWriteFile(join(tmpDir, 'subdir', 'deep', 'delta.txt'), 'deep nested\n');

    const base = '/testdir';
    await testVfs.createDir(base);
    await testVfs.createDir(`${base}/subdir`);
    await testVfs.createDir(`${base}/subdir/deep`);
    await testVfs.writeFile(`${base}/alpha.txt`, 'hello\n');
    await testVfs.writeFile(`${base}/beta.txt`, 'world!\n');
    await testVfs.writeFile(`${base}/subdir/gamma.txt`, 'nested file\n');
    await testVfs.writeFile(`${base}/subdir/deep/delta.txt`, 'deep nested\n');

    return { nativeDir: tmpDir, vfsBase: base };
  }

  it.skipIf(tier4Skip)('c-ls: directory listing with file sizes matches', async () => {
    const { nativeDir } = await setupTestTree(vfs);
    try {
      const native = await runNative('c-ls', [nativeDir]);
      const wasm = await kernel.exec('c-ls /testdir');

      expect(wasm.exitCode).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(wasm.stdout).toBe(native.stdout);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      // Verify expected entries
      expect(wasm.stdout).toContain('alpha.txt');
      expect(wasm.stdout).toContain('subdir');
    } finally {
      await rm(nativeDir, { recursive: true });
    }
  });

  it.skipIf(tier4Skip)('c-tree: recursive directory listing matches', async () => {
    const { nativeDir } = await setupTestTree(vfs);
    try {
      const native = await runNative('c-tree', [nativeDir]);
      const wasm = await kernel.exec('c-tree /testdir');

      expect(wasm.exitCode).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      // Root path (first line) differs — normalize it
      const normalizeRoot = (out: string) => out.replace(/^.+\n/, 'ROOT\n');
      expect(normalizeRoot(wasm.stdout)).toBe(normalizeRoot(native.stdout));
      // Verify tree structure present
      expect(wasm.stdout).toContain('alpha.txt');
      expect(wasm.stdout).toContain('deep');
      expect(wasm.stdout).toContain('delta.txt');
    } finally {
      await rm(nativeDir, { recursive: true });
    }
  });

  it.skipIf(tier4Skip)('c-find: find files matching glob pattern', async () => {
    const { nativeDir } = await setupTestTree(vfs);
    try {
      const native = await runNative('c-find', [nativeDir, '*.txt']);
      const wasm = await kernel.exec('c-find /testdir "*.txt"');

      expect(wasm.exitCode).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      // Paths have different roots — strip root prefix, compare relative paths
      const relPaths = (out: string, root: string) =>
        out.split('\n').filter(Boolean).map((l) => l.replace(root, '')).sort().join('\n');
      expect(relPaths(wasm.stdout, '/testdir')).toBe(relPaths(native.stdout, nativeDir));
      // Should find all 4 .txt files
      expect(wasm.stdout.split('\n').filter(Boolean)).toHaveLength(4);
    } finally {
      await rm(nativeDir, { recursive: true });
    }
  });

  it.skipIf(tier4Skip)('c-cp: copied file contents match', async () => {
    const srcContent = 'copy test content\nwith multiple lines\n';

    // Native: write source, copy, read dest
    const tmpDir = await mkdtemp(join(tmpdir(), 'c-parity-cp-'));
    try {
      const nativeSrc = join(tmpDir, 'src.txt');
      const nativeDst = join(tmpDir, 'dst.txt');
      await fsWriteFile(nativeSrc, srcContent);
      const native = await runNative('c-cp', [nativeSrc, nativeDst]);
      const nativeCopied = await fsReadFile(nativeDst, 'utf8');

      // WASM: write source to VFS, copy, read dest from VFS
      await vfs.writeFile('/tmp/src.txt', srcContent);
      const wasm = await kernel.exec('c-cp /tmp/src.txt /tmp/dst.txt');
      const wasmCopied = await vfs.readTextFile('/tmp/dst.txt');

      expect(wasm.exitCode).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasmCopied).toBe(nativeCopied);
      expect(wasmCopied).toBe(srcContent);
      // Stdout message paths differ — just verify both report success
      expect(wasm.stdout).toContain('copied:');
      expect(native.stdout).toContain('copied:');
    } finally {
      await rm(tmpDir, { recursive: true });
    }
  });

  // --- Tier 5: vendored libraries ---

  const hasCTier5Binaries = existsSync(join(C_BUILD_DIR, 'json_parse'));
  const hasCTier5Native = existsSync(join(NATIVE_DIR, 'json_parse'));
  const tier5Skip = (!hasCTier5Binaries || !hasCTier5Native)
    ? 'C Tier 5 binaries not built (run make -C native/wasmvm/c programs && make -C native/wasmvm/c native)'
    : false;

  const hasSqliteBinary = existsSync(join(C_BUILD_DIR, 'sqlite3_mem'));
  const hasSqliteNative = existsSync(join(NATIVE_DIR, 'sqlite3_mem'));
  const sqliteSkip = (!hasSqliteBinary || !hasSqliteNative)
    ? 'SQLite binaries not built (run make -C native/wasmvm/c programs && make -C native/wasmvm/c native)'
    : false;

  it.skipIf(sqliteSkip)('sqlite3_mem: in-memory SQL operations parity', async () => {
    const native = await runNative('sqlite3_mem');
    const wasm = await kernel.exec('sqlite3_mem');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Verify key structural elements
    expect(wasm.stdout).toContain('db: open');
    expect(wasm.stdout).toContain('table: created');
    expect(wasm.stdout).toContain('rows: 4');
    expect(wasm.stdout).toContain('name=Alice|score=95.5');
    expect(wasm.stdout).toContain('name=Charlie|score=NULL');
    expect(wasm.stdout).toContain('avg_score=');
    expect(wasm.stdout).toContain('db: closed');
  });

  it.skipIf(tier5Skip)('json_parse: cJSON parse and format parity', async () => {
    const sampleJson = JSON.stringify({
      name: 'agent-os',
      version: 2,
      enabled: true,
      tags: ['alpha', 'beta'],
      config: { debug: false, timeout: null, ratio: 3.14 },
      empty_arr: [],
      empty_obj: {},
    });

    const native = await runNative('json_parse', [], { input: sampleJson });
    const wasm = await kernel.exec('json_parse', { stdin: sampleJson });

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Verify key structural elements are present
    expect(wasm.stdout).toContain('"name": "agent-os"');
    expect(wasm.stdout).toContain('"enabled": true');
    expect(wasm.stdout).toContain('"timeout": null');
    expect(wasm.stdout).toContain('"ratio": 3.14');
    expect(wasm.stdout).toContain('[]');
    expect(wasm.stdout).toContain('{}');
  });

  // --- Tier 6: networking (patched sysroot + host_net) ---

  const hasCNetBinaries = existsSync(join(C_BUILD_DIR, 'tcp_echo'));
  const hasNativeNetBinaries = existsSync(join(NATIVE_DIR, 'tcp_echo'));
  const netSkip = (!hasCNetBinaries || !hasNativeNetBinaries)
    ? 'C networking binaries not built (need patched sysroot: make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs && make -C native/wasmvm/c native)'
    : false;

  it.skipIf(netSkip)('tcp_echo: connect to TCP echo server, send and receive', async () => {
    // Start a local TCP echo server
    const server = createTcpServer((conn) => {
      conn.on('data', (data) => { conn.write(data); conn.end(); });
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const port = (server.address() as import('node:net').AddressInfo).port;

    try {
      const native = await runNative('tcp_echo', [String(port)]);
      const wasm = await kernel.exec(`tcp_echo ${port}`);

      expect(wasm.exitCode).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(wasm.stdout).toBe(native.stdout);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toContain('sent: 5');
      expect(wasm.stdout).toContain('received: hello');
    } finally {
      server.close();
    }
  });

  it.skipIf(netSkip)('http_get: connect to HTTP server, receive response body', async () => {
    // Start a local HTTP server
    const server = createHttpServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      res.end('hello from http');
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const port = (server.address() as import('node:net').AddressInfo).port;

    try {
      const native = await runNative('http_get', [String(port)]);
      const wasm = await kernel.exec(`http_get ${port}`);

      expect(wasm.exitCode).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(wasm.stdout).toBe(native.stdout);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toContain('body: hello from http');
    } finally {
      server.close();
    }
  });

  it.skipIf(netSkip)('dns_lookup: resolve localhost to 127.0.0.1', async () => {
    const native = await runNative('dns_lookup', ['localhost']);
    const wasm = await kernel.exec('dns_lookup localhost');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('host: localhost');
    expect(wasm.stdout).toContain('ip: 127.0.0.1');
  });
});
