/**
 * C parity tests — native vs WASM
 *
 * Compiles C test fixtures to both native and WASM, runs both, and
 * compares stdout/stderr/exit code for parity. Tests skip when
 * WASM binaries (make wasm), C WASM binaries (make -C native/wasmvm/c programs),
 * or native binaries (make -C native/wasmvm/c native) are not built.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agentos-test-harness';
import {
  COMMANDS_DIR,
  C_BUILD_DIR,
  createKernel,
  describeIf,
  hasWasmBinaries,
  itIf,
} from '@rivet-dev/agentos-test-harness';
import type { Kernel } from '@rivet-dev/agentos-test-harness';
import { existsSync } from 'node:fs';
import { writeFile as fsWriteFile, readFile as fsReadFile, mkdtemp, rm, mkdir as fsMkdir } from 'node:fs/promises';
import { spawn, spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer as createTcpServer } from 'node:net';
import { createServer as createHttpServer } from 'node:http';

const NATIVE_DIR = join(C_BUILD_DIR, 'native');
const NATIVE_FIXTURE_NAMES: Readonly<Record<string, string>> = {
  cat: 'c-cat',
  env: 'c-env',
  sort: 'c-sort',
  wc: 'c-wc',
};

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
  return new Promise((res, reject) => {
    const fixtureName = NATIVE_FIXTURE_NAMES[name] ?? name;
    const proc = spawn(join(NATIVE_DIR, fixtureName), args, {
      env: options?.env,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';

    proc.stdout.on('data', (d: Buffer) => { stdout += d.toString(); });
    proc.stderr.on('data', (d: Buffer) => { stderr += d.toString(); });
    proc.on('error', reject);

    if (options?.input !== undefined) {
      proc.stdin.write(options.input);
    }
    proc.stdin.end();

    proc.on('close', (code) => {
      res({ exitCode: code ?? 0, stdout, stderr });
    });
  });
}

function runNativeWithHosts(
  name: string,
  hostsFile: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((res, reject) => {
    const proc = spawn('unshare', [
      '-Urm',
      'sh',
      '-c',
      'mount --bind "$1" /etc/hosts && exec "$2"',
      'sh',
      hostsFile,
      join(NATIVE_DIR, name),
    ], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';

    proc.stdout.on('data', (d: Buffer) => { stdout += d.toString(); });
    proc.stderr.on('data', (d: Buffer) => { stderr += d.toString(); });
    proc.on('error', reject);
    proc.on('close', (code) => res({ exitCode: code ?? 0, stdout, stderr }));
  });
}

function runNativeWithNetworkFiles(
  name: string,
  hostsFile: string,
  servicesFile: string,
  args: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((res, reject) => {
    const proc = spawn('unshare', [
      '-Urm',
      'sh',
      '-c',
      'mount --bind "$1" /etc/hosts && mount --bind "$2" /etc/services && binary=$3 && shift 3 && exec "$binary" "$@"',
      'sh',
      hostsFile,
      servicesFile,
      join(NATIVE_DIR, name),
      ...args,
    ], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    proc.stdout.on('data', (d: Buffer) => { stdout += d.toString(); });
    proc.stderr.on('data', (d: Buffer) => { stderr += d.toString(); });
    proc.on('error', reject);
    proc.on('close', (code) => res({ exitCode: code ?? 0, stdout, stderr }));
  });
}

function runNativeWithLibcFiles(
  name: string,
  hostsFile: string,
  servicesFile: string,
  passwdFile: string,
  args: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((res, reject) => {
    const proc = spawn('unshare', [
      '-Urm',
      'sh',
      '-c',
      'mount --bind "$1" /etc/hosts && mount --bind "$2" /etc/services && mount --bind "$3" /etc/passwd && binary=$4 && shift 4 && exec "$binary" "$@"',
      'sh',
      hostsFile,
      servicesFile,
      passwdFile,
      join(NATIVE_DIR, name),
      ...args,
    ], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    proc.stdout.on('data', (d: Buffer) => { stdout += d.toString(); });
    proc.stderr.on('data', (d: Buffer) => { stderr += d.toString(); });
    proc.on('error', reject);
    proc.on('close', (code) => res({ exitCode: code ?? 0, stdout, stderr }));
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
  private metadata = new Map<string, { mode: number; uid: number; gid: number }>();

  private resolve(path: string): string {
    const target = this.symlinks.get(path);
    if (!target) return path;
    return target.startsWith('/') ? target : join(path, '..', target);
  }

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
  async pwrite(path: string, offset: number, content: Uint8Array): Promise<void> {
    const data = await this.readFile(path);
    const next = new Uint8Array(Math.max(data.length, offset + content.length));
    next.set(data);
    next.set(content, offset);
    this.files.set(path, next);
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
    if (!this.metadata.has(path)) {
      this.metadata.set(path, { mode: 0o100644, uid: 1000, gid: 1000 });
    }
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
    return this.statEntry(this.resolve(path));
  }
  async lstat(path: string) {
    return this.statEntry(path);
  }
  private async statEntry(path: string) {
    const isDir = this.dirs.has(path);
    const isSymlink = this.symlinks.has(path);
    const data = this.files.get(path);
    if (!isDir && !isSymlink && !data) throw new Error(`ENOENT: ${path}`);
    const metadata = this.metadata.get(path) ?? {
      mode: isSymlink ? 0o120777 : (isDir ? 0o40755 : 0o100644),
      uid: 1000,
      gid: 1000,
    };
    return {
      mode: metadata.mode,
      size: data?.length ?? 0,
      isDirectory: isDir,
      isSymbolicLink: isSymlink,
      atimeMs: Date.now(),
      mtimeMs: Date.now(),
      ctimeMs: Date.now(),
      birthtimeMs: Date.now(),
      ino: 0,
      nlink: 1,
      uid: metadata.uid,
      gid: metadata.gid,
    };
  }
  async chmod(path: string, mode: number) {
    const resolved = this.resolve(path);
    const stat = await this.statEntry(resolved);
    this.metadata.set(resolved, {
      mode: (stat.mode & 0o170000) | (mode & 0o7777),
      uid: stat.uid,
      gid: stat.gid,
    });
  }
  async chown(
    path: string,
    uid: number,
    gid: number,
    options?: { followSymlinks?: boolean },
  ) {
    const resolved = options?.followSymlinks === false ? path : this.resolve(path);
    const stat = await this.statEntry(resolved);
    this.metadata.set(resolved, { mode: stat.mode, uid, gid });
  }
  async rename(from: string, to: string) {
    const data = this.files.get(from);
    if (data) { this.files.set(to, data); this.files.delete(from); }
    const metadata = this.metadata.get(from);
    if (metadata) { this.metadata.set(to, metadata); this.metadata.delete(from); }
  }
  async unlink(path: string) {
    this.files.delete(path);
    this.symlinks.delete(path);
    this.metadata.delete(path);
  }
  async rmdir(path: string) { this.dirs.delete(path); }
  async symlink(target: string, linkPath: string) {
    this.symlinks.set(linkPath, target);
    this.metadata.set(linkPath, { mode: 0o120777, uid: 1000, gid: 1000 });
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

describeIf(!skipReason(), 'C parity: native vs WASM', { timeout: 30_000 }, () => {
  let kernel: Kernel;
  let vfs: SimpleVFS;

  async function mountParityKernel(options: { loopbackExemptPorts?: number[] } = {}) {
    const nextKernel = createKernel({
      filesystem: vfs as any,
      ...(options.loopbackExemptPorts
        ? { loopbackExemptPorts: options.loopbackExemptPorts }
        : {}),
    });
    // C build dir first so C programs take precedence over same-named Rust commands
    await nextKernel.mount(createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }));
    return nextKernel;
  }

  async function recreateKernel(options: { loopbackExemptPorts?: number[] } = {}) {
    await kernel?.dispose();
    kernel = await mountParityKernel(options);
  }

  beforeEach(async () => {
    vfs = new SimpleVFS();
    kernel = await mountParityKernel();
  });

  afterEach(async () => {
    await kernel?.dispose();
  });

  // --- Tier 1: basic I/O ---

  it('hello: stdout and exit code match', async () => {
    const native = await runNative('hello');
    const wasm = await kernel.exec('hello');

    expect(
      wasm.exitCode,
      `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
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
    const counts = (output: string) => output.trim().split(/\s+/).map(Number);
    expect(counts(wasm.stdout)).toEqual(counts(native.stdout));
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

  itIf(!tier2Skip, 'isatty_test: piped stdin/stdout/stderr all report not-a-tty', async () => {
    const native = await runNative('isatty_test');
    const wasm = await kernel.exec('isatty_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
  });

  itIf(!tier2Skip, 'getpid_test: PID is valid, not hardcoded 42, and consistent', async () => {
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

  itIf(!tier2Skip, 'getppid_test: top-level parent PID is valid', async () => {
    const native = await runNative('getppid_test');
    const wasm = await kernel.exec('getppid_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('ppid_nonnegative=yes');
    expect(native.stdout).toContain('ppid_nonnegative=yes');
    expect(wasm.stdout).toContain('ppid=0');
  });

  itIf(!tier2Skip, 'userinfo: uid/gid/euid/egid values are specific', async () => {
    const native = await runNative('userinfo');
    const wasm = await kernel.exec('userinfo');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    // Verify format for both
    const format = /^uid=\d+\ngid=\d+\neuid=\d+\negid=\d+\nsetgroups_unprivileged_eperm=yes\n$/;
    expect(wasm.stdout).toMatch(format);
    expect(native.stdout).toMatch(format);
    // WASM kernel returns uid/gid = 1000 (sandbox user)
    expect(wasm.stdout).toContain('uid=1000');
    expect(wasm.stdout).toContain('gid=1000');
    expect(wasm.stdout).toContain('euid=1000');
    expect(wasm.stdout).toContain('egid=1000');
    expect(wasm.stdout).toContain('setgroups_unprivileged_eperm=yes');
  });

  itIf(!tier2Skip, 'getpwuid_test: passwd entry fields valid', async () => {
    const native = await runNative('getpwuid_test');
    const wasm = await kernel.exec('getpwuid_test');

    const diagnostic =
      `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}` +
      `\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`;
    expect(wasm.exitCode, diagnostic).toBe(native.exitCode);
    expect(wasm.exitCode, diagnostic).toBe(0);
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

  itIf(!tier2Skip, 'libc compatibility: live databases, shell pipes, and syslog match Linux', async () => {
    // The parity harness intentionally starts with an empty injected VFS.
    // Populate the two identity databases and verify libc reads them live.
    await vfs.writeFile('/etc/passwd', 'root:x:0:0:root:/root:/bin/sh\n');
    await vfs.writeFile('/etc/group', 'root:x:0:root\n');
    const native = await runNative('libc_compat_contract');
    const wasm = await kernel.exec('libc_compat_contract');
    const normalizeSyslogPid = (value: string) =>
      normalizeStderr(value)
        .replace(/^\[agentos\] WASM open fd usage .*\n/gm, '')
        .replace(/libc-compat\[\d+\]/g, 'libc-compat[pid]');

    expect(wasm.exitCode, `${wasm.stderr}\n${wasm.stdout}`).toBe(native.exitCode);
    expect(wasm.exitCode, `${wasm.stderr}\n${wasm.stdout}`).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeSyslogPid(wasm.stderr)).toBe(normalizeSyslogPid(native.stderr));
    expect(wasm.stdout).toContain('host_lookup=yes');
    expect(wasm.stdout).toContain('service_lookup=yes');
    expect(wasm.stdout).toContain('passwd_lookup=yes');
    expect(wasm.stdout).toContain('group_lookup=yes');
    expect(wasm.stdout).toContain('group_enumeration=yes');
    expect(wasm.stdout).toContain('system_shell=yes');
    expect(wasm.stdout).toContain('popen_read=yes');
    expect(wasm.stdout).toContain('popen_write=yes');
    expect(wasm.stdout).toContain('pclose_close_error=yes');
    expect(wasm.stdout).toContain('setrlimit_truthful=yes');
    expect(wasm.stdout).toContain('setrlimit_hard_raise_denied=yes');
    expect(wasm.stderr).toContain('syslog-visible=17');
  });

  itIf(!tier2Skip, 'libc bounds: fd limit, large host sets, and oversized databases fail explicitly', async () => {
    const hosts = Array.from(
      { length: 20 },
      (_, index) => `198.51.100.${index + 1} many.test`,
    ).join('\n') + '\n';
    const services = `oversvc 12345/tcp ${'alias'.repeat(240)}\n`;
    const passwdPrefix = 'oversuser:x:123:456:';
    const passwdSuffix = ':/home/oversuser:/bin/sh';
    const passwd = `${passwdPrefix}${'g'.repeat(
      4096 - passwdPrefix.length - passwdSuffix.length,
    )}${passwdSuffix}\n`;
    expect(Buffer.byteLength(passwd.slice(0, -1))).toBe(4096);
    await vfs.writeFile('/etc/hosts', hosts);
    await vfs.writeFile('/etc/services', services);
    await vfs.writeFile('/etc/passwd', passwd);

    const wasm = await kernel.exec('libc_bounds_contract many.test oversvc oversuser');
    expect(wasm.exitCode, `${wasm.stderr}\n${wasm.stdout}`).toBe(0);
    expect(wasm.stderr).toBe('');
    expect(wasm.stdout).toContain('nofile_soft=1024\n');
    expect(wasm.stdout).toContain('nofile_hard=1024\n');
    expect(wasm.stdout).toContain('host_addresses=20\n');
    expect(wasm.stdout).toContain('service_found=no\nservice_erange=yes\n');
    expect(wasm.stdout).toContain('passwd_found=no\npasswd_erange=yes\n');

    const overflowHosts = Array.from(
      { length: 65 },
      (_, index) => `203.0.113.${(index % 254) + 1} overflow.test`,
    ).join('\n') + '\n';
    await vfs.writeFile('/etc/hosts', overflowHosts);
    const overflow = await kernel.exec(
      'libc_bounds_contract overflow.test oversvc oversuser',
    );
    expect(overflow.exitCode, `${overflow.stderr}\n${overflow.stdout}`).toBe(0);
    expect(overflow.stderr).toBe('');
    expect(overflow.stdout).toContain(
      'host_addresses=error\nhost_erange=yes\nhost_no_recovery=yes\n',
    );

    const nativeDir = await mkdtemp(join(tmpdir(), 'agentos-libc-bounds-'));
    const nativeHosts = join(nativeDir, 'hosts');
    const nativeServices = join(nativeDir, 'services');
    const nativePasswd = join(nativeDir, 'passwd');
    try {
      await fsWriteFile(nativeHosts, hosts);
      await fsWriteFile(nativeServices, services);
      await fsWriteFile(nativePasswd, passwd);
      const probe = spawnSync('unshare', [
        '-Urm', 'sh', '-c',
        'mount --bind "$1" /etc/hosts && mount --bind "$2" /etc/services && mount --bind "$3" /etc/passwd',
        'sh', nativeHosts, nativeServices, nativePasswd,
      ], { stdio: 'ignore' });
      if (probe.status === 0) {
        const native = await runNativeWithLibcFiles(
          'libc_bounds_contract',
          nativeHosts,
          nativeServices,
          nativePasswd,
          ['many.test', 'oversvc', 'oversuser'],
        );
        expect(native.exitCode, `${native.stderr}\n${native.stdout}`).toBe(0);
        expect(native.stderr).toBe('');
        expect(native.stdout).toContain('host_addresses=20\n');
        expect(native.stdout).toContain('service_found=yes\nservice_erange=no\n');
        expect(native.stdout).toContain('passwd_found=yes\npasswd_erange=no\n');

        await fsWriteFile(nativeHosts, overflowHosts);
        const nativeOverflow = await runNativeWithLibcFiles(
          'libc_bounds_contract',
          nativeHosts,
          nativeServices,
          nativePasswd,
          ['overflow.test', 'oversvc', 'oversuser'],
        );
        expect(
          nativeOverflow.exitCode,
          `${nativeOverflow.stderr}\n${nativeOverflow.stdout}`,
        ).toBe(0);
        expect(nativeOverflow.stderr).toBe('');
        expect(nativeOverflow.stdout).toContain('host_addresses=65\n');
      }
    } finally {
      await rm(nativeDir, { recursive: true, force: true });
    }
  });

  itIf(!tier2Skip, 'libc group bounds: exact capacities succeed and overflow is explicit without overwrite', async () => {
    const members = Array.from({ length: 256 }, (_, index) => `u${index}`);
    await vfs.writeFile('/etc/group', `membercap:x:2000:${members.join(',')}\n`);
    const exactMembers = await kernel.exec('getgrouplist_bounds group-members');
    expect(exactMembers.exitCode, `${exactMembers.stderr}\n${exactMembers.stdout}`).toBe(0);
    expect(exactMembers.stderr).toBe('');
    expect(exactMembers.stdout).toBe(
      'group_found=yes\ngroup_members=256\ngroup_overflow=no\n',
    );

    await vfs.writeFile(
      '/etc/group',
      `membercap:x:2000:${[...members, 'overflow'].join(',')}\n`,
    );
    const overflowMembers = await kernel.exec('getgrouplist_bounds group-members');
    expect(
      overflowMembers.exitCode,
      `${overflowMembers.stderr}\n${overflowMembers.stdout}`,
    ).toBe(0);
    expect(overflowMembers.stderr).toBe('');
    expect(overflowMembers.stdout).toBe(
      'group_found=no\ngroup_members=0\ngroup_overflow=yes\n',
    );

    const matchingGroups = (count: number) =>
      Array.from(
        { length: count },
        (_, index) => `g${index}:x:${2000 + index}:boundsuser`,
      ).join('\n') + '\n';
    await vfs.writeFile('/etc/group', matchingGroups(255));
    const exactList = await kernel.exec('getgrouplist_bounds grouplist');
    expect(exactList.exitCode, `${exactList.stderr}\n${exactList.stdout}`).toBe(0);
    expect(exactList.stderr).toBe('');
    expect(exactList.stdout).toBe(
      'grouplist_result=256\ngrouplist_count=256\ngrouplist_overflow=no\ngrouplist_canary=yes\n',
    );

    await vfs.writeFile('/etc/group', matchingGroups(256));
    const matchingOverflow = await kernel.exec('getgrouplist_bounds grouplist');
    expect(
      matchingOverflow.exitCode,
      `${matchingOverflow.stderr}\n${matchingOverflow.stdout}`,
    ).toBe(0);
    expect(matchingOverflow.stderr).toBe('');
    expect(matchingOverflow.stdout).toContain('grouplist_result=-1\n');
    expect(matchingOverflow.stdout).toContain('grouplist_overflow=yes\n');
    expect(matchingOverflow.stdout).toContain('grouplist_canary=yes\n');

    const nonmatchingGroups = Array.from(
      { length: 257 },
      (_, index) => `g${index}:x:${2000 + index}:someoneelse`,
    ).join('\n') + '\n';
    await vfs.writeFile('/etc/group', nonmatchingGroups);
    const databaseOverflow = await kernel.exec('getgrouplist_bounds grouplist');
    expect(
      databaseOverflow.exitCode,
      `${databaseOverflow.stderr}\n${databaseOverflow.stdout}`,
    ).toBe(0);
    expect(databaseOverflow.stderr).toBe('');
    expect(databaseOverflow.stdout).toContain('grouplist_result=-1\n');
    expect(databaseOverflow.stdout).toContain('grouplist_overflow=yes\n');
    expect(databaseOverflow.stdout).toContain('grouplist_canary=yes\n');
  });

  itIf(!tier2Skip, 'chown family matches Linux ownership, fd, and symlink semantics', async () => {
    const native = await runNative('chown_contract');
    const wasm = await kernel.exec('chown_contract');

    expect(wasm.exitCode, `${wasm.stderr}\n${wasm.stdout}`).toBe(native.exitCode);
    expect(wasm.exitCode, `${wasm.stderr}\n${wasm.stdout}`).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('lchown_preserved_target=yes');
    expect(wasm.stdout).toContain('chown_followed_target=yes');
    expect(wasm.stdout).toContain('unchanged_ids_preserved=yes');
    expect(wasm.stdout).toContain('nonexec_setgid_preserved=yes');
    expect(wasm.stdout).toContain('foreign_uid_eperm=yes');
    expect(wasm.stdout).toContain('fchownat_empty_path=yes');
    expect(wasm.stdout).toContain('detached_directory_fchmod=yes');
    expect(wasm.stdout).toContain('pipe_fchmod=yes');
    expect(wasm.stdout).toContain('socket_fchmod=yes');
    expect(wasm.stdout).toContain('fchownat_invalid_flags=yes');
  });

  itIf(!tier2Skip, 'pipe_test: write through pipe and read back matches', async () => {
    const native = await runNative('pipe_test');
    const wasm = await kernel.exec('pipe_test');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
  });

  itIf(!tier2Skip, 'dup_test: write through duplicated fds matches', async () => {
    const native = await runNative('dup_test');
    const wasm = await kernel.exec('dup_test');

    const diagnostic = `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`;
    expect(wasm.exitCode, diagnostic).toBe(native.exitCode);
    expect(wasm.stdout, diagnostic).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr), diagnostic).toBe(normalizeStderr(native.stderr));
  });

  itIf(!tier2Skip, 'closefrom_test: closes high virtual descriptors', async () => {
    const native = await runNative('closefrom_test');
    const wasm = await kernel.exec('closefrom_test');

    const diagnostic = `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`;
    expect(wasm.exitCode, diagnostic).toBe(native.exitCode);
    expect(wasm.exitCode, diagnostic).toBe(0);
    expect(wasm.stdout, diagnostic).toBe(native.stdout);
    expect(wasm.stdout, diagnostic).toContain('closefrom_closed=yes');
    expect(normalizeStderr(wasm.stderr), diagnostic).toBe(normalizeStderr(native.stderr));
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

  itIf(!tier3Skip, 'spawn_child: posix_spawn echo, capture stdout via pipe', async () => {
    const native = await runNative('spawn_child');
    const wasm = await kernel.exec('spawn_child');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('child_stdout: hello');
    expect(wasm.stdout).toContain('child_exit: 0');
    expect(wasm.stdout).toContain('custom_argv0=custom-argv0');
    expect(wasm.stdout).toContain('empty_len=0 tail=tail');
    expect(wasm.stdout).toContain('spawn_bare_enoent: yes');
    expect(wasm.stdout).toContain('spawn_empty_enoent: yes');
  });

  itIf(!tier3Skip, 'spawn_contract: ordered file actions, inherited descriptions, and signal semantics match Linux', async () => {
    const native = await runNative('spawn_contract');
    const wasm = await kernel.exec('spawn_contract');

    expect(wasm.exitCode, `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('spawn_actions_ordered=yes');
    expect(wasm.stdout).toContain('spawn_actions_parent_unchanged=yes');
    expect(wasm.stdout).toContain('spawn_closefrom_ordered=yes');
    expect(wasm.stdout).toContain('spawn_closefrom_public_preopen_hidden=yes');
    expect(wasm.stdout).toContain('spawn_inherit_shared_description=yes');
    expect(wasm.stdout).toContain('spawn_same_fd_dup2_clears_cloexec=yes');
    expect(wasm.stdout).toContain('spawn_live_cwd_inherited=yes');
    expect(wasm.stdout).toContain('spawn_chdir_symlink_relative_open=yes');
    expect(wasm.stdout).toContain('spawn_fchdir_symlink_relative_open=yes');
    expect(wasm.stdout).toContain('spawnp_path_actions_once=yes');
    expect(wasm.stdout).toContain('spawnp_empty_path_current_directory=yes');
    expect(wasm.stdout).toContain('signal_mask_unmaskable=yes');
    expect(wasm.stdout).toContain('signal_mask_pending_coalesced=yes');
    expect(wasm.stdout).toContain('spawn_signal_masks=yes');
    expect(wasm.stdout).toContain('spawn_signal_defaults=yes');
    expect(wasm.stdout).toContain('signal_handler_mask_and_reset=yes');
    expect(wasm.stdout).toContain('spawn_pgroup=yes');
    expect(wasm.stdout).toContain('spawn_resetids=yes');
    expect(wasm.stdout).toContain('spawn_setsid=yes');
    expect(wasm.stdout).toContain('spawn_scheduler_attrs=yes');
    expect(wasm.stdout).toContain('spawn_bidirectional_pipes=yes');
    expect(wasm.stdout).toContain('waitpid_schedules_sibling=yes');
    expect(wasm.stdout).toContain('spawn_closefrom_pipe_writer_eof=yes');
    expect(wasm.stdout).toContain('spawn_retained_writer_not_inherited=yes');
    expect(wasm.stdout).toContain('spawn_borrows_parent_stdout=yes');
    expect(wasm.stdout).toContain('spawn_closed_stdout_ebadf=yes');
    expect(wasm.stdout).toContain('stdio_close_restore=yes');
    expect(wasm.stdout).toContain('spawn_contract=ok');
  });

  const hasOpenFlagsContract =
    existsSync(join(C_BUILD_DIR, 'open_flags')) &&
    existsSync(join(NATIVE_DIR, 'open_flags'));
  itIf(
    hasOpenFlagsContract,
    'open_flags: directory and nofollow failures match Linux without side effects',
    async () => {
      const native = await runNative('open_flags');
      const wasm = await kernel.exec('open_flags');

      expect(
        wasm.exitCode,
        `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
      ).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toBe(native.stdout);
      expect(wasm.stdout).toContain('open_directory_truncate_enotdir=yes');
      expect(wasm.stdout).toContain('open_nofollow_truncate_eloop=yes');
      expect(wasm.stdout).toContain('open_directory_create_einval=yes');
      expect(wasm.stdout).toContain('open_failure_side_effects_absent=yes');
      expect(wasm.stdout).toContain('umask_applies_to_kernel_create=yes');
      expect(wasm.stdout).toContain('open_flags=ok');
    },
  );

  const hasReaddirContract =
    existsSync(join(C_BUILD_DIR, 'readdir_contract')) &&
    existsSync(join(NATIVE_DIR, 'readdir_contract'));
  itIf(
    hasReaddirContract,
    'readdir_contract: dot entries, inodes, and multi-page cookies match Linux',
    async () => {
      const native = await runNative('readdir_contract');
      const wasm = await kernel.exec('readdir_contract');

      expect(
        wasm.exitCode,
        `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
      ).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toBe(native.stdout);
      expect(wasm.stdout).toContain('readdir_dots=yes');
      expect(wasm.stdout).toContain('readdir_nonzero_ino=yes');
      expect(wasm.stdout).toContain('readdir_ino_matches_stat=yes');
      expect(wasm.stdout).toContain('readdir_seekdir_resume=yes');
      expect(wasm.stdout).toContain('readdir_short_buffer_cookie=yes');
      expect(wasm.stdout).toContain('readdir_stable_ino=yes');
      expect(wasm.stdout).toContain('readdir_detached_directory=yes');
      expect(wasm.stdout).toContain('proc_deleted_file=yes');
      expect(wasm.stdout).toContain('readdir_renamed_directory=yes');
      expect(wasm.stdout).toContain('readdir_fdopendir_first_read_and_eof=yes');
      expect(wasm.stdout).toContain('readdir_count=222');
      expect(wasm.stdout).toContain('readdir_contract=ok');
    },
  );

  const hasRecordLockContract =
    existsSync(join(C_BUILD_DIR, 'record_lock')) &&
    existsSync(join(NATIVE_DIR, 'record_lock'));

  const hasMlockContract =
    existsSync(join(C_BUILD_DIR, 'mlock_contract')) &&
    existsSync(join(NATIVE_DIR, 'mlock_contract'));
  itIf(
    hasMlockContract,
    'madvise_contract: advisory hints and validation match Linux',
    async () => {
      const native = await runNative('mlock_contract');
      const wasm = await kernel.exec('mlock_contract');

      expect(
        wasm.exitCode,
        `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
      ).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(wasm.stdout).toBe(native.stdout);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toContain('madvise_unmapped_tail_enomem=yes');
      expect(wasm.stdout).toContain('madvise_hints_and_validation=yes');
      expect(wasm.stdout).toContain('memory_lock_range_validation=yes');
      expect(wasm.stdout).toContain('madvise_contract=ok');
    },
  );

  itIf(
    hasMlockContract,
    'mlock_contract: unsupported WASM residency operations fail explicitly',
    async () => {
      const native = await runNative('mlock_contract', ['--linux-specific']);
      const wasm = await kernel.exec('mlock_contract --wasm-specific');

      expect(native.exitCode, native.stderr).toBe(0);
      expect(native.stdout).toContain('mlock_linux=yes');
      expect(native.stdout).toContain('mlockall_future_linux=yes');
      expect(native.stdout).toContain('madvise_dontneed_linux=yes');
      expect(wasm.exitCode, wasm.stderr).toBe(0);
      expect(wasm.stdout).toContain('memory_locking_unsupported=yes');
      expect(wasm.stdout).toContain('madvise_dontneed_unsupported=yes');
    },
  );

  itIf(
    hasRecordLockContract,
    'record_lock: POSIX conflicts, byte ranges, and close/exit release match Linux',
    async () => {
      const native = await runNative('record_lock');
      const wasm = await kernel.exec('record_lock');

      expect(
        wasm.exitCode,
        `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
      ).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(wasm.stdout).toBe(native.stdout);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toContain('record_lock_conflict_and_ranges=yes');
      expect(wasm.stdout).toContain('record_lock_any_close_releases=yes');
      expect(wasm.stdout).toContain('record_lock_exit_releases=yes');
      expect(wasm.stdout).toContain('record_lock_setlkw_immediate=yes');
      expect(wasm.stdout).toContain('record_lock_setlkw_deadlock=yes');
      expect(wasm.stdout).toContain('record_lock_setlkw_wakeup=yes');
      expect(wasm.stdout).toContain('record_lock_setlkw_eintr=yes');
      expect(wasm.stdout).toContain('record_lock_setlkw_sibling_wakeup=yes');
      expect(wasm.stdout).toContain('record_lock=ok');
    },
  );

  itIf(!tier3Skip, 'spawn_contract: bidirectional POSIX pipes deliver repeated reads and final EOF', async () => {
    const native = await runNative('spawn_contract', ['--bidirectional-parent']);
    const wasm = await kernel.exec('spawn_contract --bidirectional-parent');

    expect(wasm.exitCode, `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`).toBe(
      native.exitCode,
    );
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toBe('spawn_bidirectional_pipes=yes\n');
  });

  itIf(!tier3Skip, 'spawn_contract: waitpid keeps non-selected siblings scheduled', async () => {
    const native = await runNative('spawn_contract', ['--waitpid-sibling-parent']);
    const wasm = await kernel.exec('spawn_contract --waitpid-sibling-parent');

    expect(wasm.exitCode, `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`).toBe(
      native.exitCode,
    );
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toBe('waitpid_schedules_sibling=yes\n');
  });

  itIf(!tier3Skip, 'spawn_contract: closefrom closes unrelated pipe writers in a later child', async () => {
    const native = await runNative('spawn_contract', ['--closefrom-pipe-parent']);
    const wasm = await kernel.exec('spawn_contract --closefrom-pipe-parent');

    expect(wasm.exitCode, `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`).toBe(
      native.exitCode,
    );
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toBe('spawn_closefrom_pipe_writer_eof=yes\n');
  });

  itIf(!tier3Skip, 'spawn_contract: retained writers owned by older children are not inherited', async () => {
    const native = await runNative('spawn_contract', ['--retained-pipe-parent']);
    const wasm = await kernel.exec('spawn_contract --retained-pipe-parent');

    expect(wasm.exitCode, `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`).toBe(
      native.exitCode,
    );
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toBe('spawn_retained_writer_not_inherited=yes\n');
  });

  const hasPpollContract =
    existsSync(join(C_BUILD_DIR, 'ppoll_contract')) &&
    existsSync(join(NATIVE_DIR, 'ppoll_contract'));
  itIf(
    hasPpollContract,
    'ppoll_contract: temporary masks, EINTR, ready results, and restoration match Linux',
    async () => {
      const native = await runNative('ppoll_contract');
      const wasm = await kernel.exec('ppoll_contract');

      expect(wasm.exitCode, `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(wasm.stdout).toBe(native.stdout);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toContain('poll_linux_abi=yes');
      expect(wasm.stdout).toContain('poll_normal_aliases=yes');
      expect(wasm.stdout).toContain('ppoll_pending_unblocked_eintr=yes');
      expect(wasm.stdout).toContain('ppoll_temporary_block_ready=yes');
      expect(wasm.stdout).toContain('ppoll_error_mask_restore=yes');
      expect(wasm.stdout).toContain('ppoll_contract=ok');
    },
  );

  itIf(!tier3Skip, 'fd_mapping_contract: sequential collisions, swaps, duplicate targets, and CLOEXEC match Linux', async () => {
    const native = await runNative('fd_mapping_contract');
    const wasm = await kernel.exec('fd_mapping_contract');

    expect(
      wasm.exitCode,
      `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(
      'sequential_collision=ok\nswap_cycle=ok\nduplicate_target=ok\n',
    );
  });

  itIf(!tier3Skip, 'spawn_exit_code: child exits non-zero, verify via waitpid', async () => {
    const native = await runNative('spawn_exit_code');
    const wasm = await kernel.exec('spawn_exit_code');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('child_exit_code: 7');
    expect(wasm.stdout).toContain('match: yes');
  });

  itIf(!tier3Skip, 'pipeline: echo hello | cat via pipe + posix_spawn', async () => {
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

  itIf(!tier3Skip, 'kill_child: spawn sleep, kill SIGTERM, verify terminated', async () => {
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

  itIf(!tier3Skip, 'signal_tests: SIGKILL, kill exited PID, kill invalid PID', async () => {
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

  itIf(!tier3Skip, 'sigaction_behavior: query, SA_RESETHAND, and SA_RESTART parity', async () => {
    const env = { ...process.env, PATH: `${NATIVE_DIR}:${process.env.PATH ?? ''}` };
    const native = await runNative('sigaction_behavior', [], { env });
    const wasm = await kernel.exec('sigaction_behavior');

    expect(
      wasm.exitCode,
      `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('sigaction_query_mask_sigterm=yes');
    expect(wasm.stdout).toContain('sigaction_query_flags=yes');
    expect(wasm.stdout).toContain('sa_resethand_handler_calls=1');
    expect(wasm.stdout).toContain('sa_resethand_reset=yes');
    expect(wasm.stdout).toContain('sa_restart_handler_calls=1');
    expect(wasm.stdout).toContain('sa_restart_accept=yes');
    expect(wasm.stdout).toContain('sa_restart_child_exit=0');
    expect(wasm.stdout).toContain('sa_restart_signal_exit=0');
  });

  itIf(!tier3Skip, 'sigaction_self: self kill dispatches SA_RESETHAND handler', async () => {
    const native = await runNative('sigaction_self');
    const wasm = await kernel.exec('sigaction_self');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('self_signal_handler_calls=1');
    expect(wasm.stdout).toContain('self_signal_reset=yes');
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
  });

  itIf(!tier3Skip, 'tcp_accept_spawn: accept spawned child connection', async () => {
    const env = { ...process.env, PATH: `${NATIVE_DIR}:${process.env.PATH ?? ''}` };
    const native = await runNative('tcp_accept_spawn', [], { env });
    const wasm = await kernel.exec('tcp_accept_spawn');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('accept_child_message=yes');
    expect(wasm.stdout).toContain('accept_child_exit=0');
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
  });

  itIf(!tier3Skip, 'getppid_verify: child getppid matches parent getpid', async () => {
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

  itIf(!tier3Skip, 'waitpid_return: waitpid returns correct child PID', async () => {
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

  itIf(!tier3Skip, 'waitpid_edge: concurrent children and invalid PID', async () => {
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
    // Test 4: a conventional exit code of 137 remains a normal exit.
    expect(wasm.stdout).toContain('test4_exited: yes');
    expect(wasm.stdout).toContain('test4_exit: 137');
    expect(wasm.stdout).toContain('test4_signaled: no');
    expect(wasm.stdout).toContain('test4: ok');
    expect(native.stdout).toContain('test4: ok');
    // Test 5: waitpid(-1) returns the child that is ready first.
    expect(wasm.stdout).toContain('test5_first_ready: yes');
    expect(wasm.stdout).toContain('test5: ok');
    expect(native.stdout).toContain('test5: ok');
    // Test 6: pid=0 selects the caller's process group.
    expect(wasm.stdout).toContain('test6_pid_zero_group: yes');
    expect(wasm.stdout).toContain('test6: ok');
    // Test 7: pid<-1 selects an explicit process group.
    expect(wasm.stdout).toContain('test7_negative_group: yes');
    expect(wasm.stdout).toContain('test7: ok');
    // Test 8: WUNTRACED/WCONTINUED preserve raw Linux state transitions.
    expect(wasm.stdout).toContain('test8_stopped: yes');
    expect(wasm.stdout).toContain('test8_continued: yes');
    expect(wasm.stdout).toContain('test8_terminated: yes');
    expect(wasm.stdout).toContain('test8: ok');
    // Test 9: invalid options are typed and do not consume the child.
    expect(wasm.stdout).toContain('test9_invalid_options: yes');
    expect(wasm.stdout).toContain('test9_child_preserved: yes');
    expect(wasm.stdout).toContain('test9: ok');
    // Test 10: SIGCHLD from a non-selected sibling interrupts waitpid(pid).
    expect(wasm.stdout).toContain('test10_sigchld_handler: yes');
    expect(wasm.stdout).toContain('test10_waitpid_eintr: yes');
    expect(wasm.stdout).toContain('test10_children_cleaned: yes');
    expect(wasm.stdout).toContain('test10: ok');
    expect(native.stdout).toContain('test10: ok');
  });

  itIf(!tier3Skip, 'itimer_contract: real timer timing, masking, and selector policy match Linux', async () => {
    const native = await runNative('itimer_contract');
    const wasm = await kernel.exec('itimer_contract');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('itimer_real_remaining=yes');
    expect(wasm.stdout).toContain('alarm_rounding=yes');
    expect(wasm.stdout).toContain('itimer_masked_coalesced=yes');
    expect(wasm.stdout).toContain('itimer_selector_policy=yes');
    expect(wasm.stdout).toContain('alarm_default_termination=yes');
    expect(wasm.stdout).toContain('itimer_contract=ok');
  });

  itIf(!tier3Skip, 'pipe_edge: large write, broken pipe, EOF, close-both', async () => {
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

  itIf(!tier3Skip, 'select_edge: one descriptor ready in multiple sets counts each ready bit', async () => {
    const native = await runNative('select_edge');
    const wasm = await kernel.exec('select_edge');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('select_ready_count: 2');
    expect(wasm.stdout).toContain('select_read_ready: yes');
    expect(wasm.stdout).toContain('select_write_ready: yes');
    expect(wasm.stdout).toContain('select_large_fdset: yes');
    expect(wasm.stdout).toContain('select_high_fd: yes');
    expect(wasm.stdout).toContain('select_cloexec_fd: yes');
    expect(wasm.stdout).toContain('select_fd_ceiling_einval: yes');
    expect(wasm.stdout).toContain('select_timeout_normalized: yes');
    expect(wasm.stdout).toContain('select_timeout_expired: yes');
    expect(wasm.stdout).toContain('select_invalid_timeout: yes');
    expect(wasm.stdout).toContain('select_invalid_nfds: yes');
    expect(wasm.stdout).toContain('select_above_nfds_ignored: yes');
    expect(wasm.stdout).toContain('select_ebadf: yes');
    expect(wasm.stdout).toContain('select_huge_empty: yes');
    expect(wasm.stdout).toContain('select_huge_sparse: yes');
    expect(wasm.stdout).toContain('select_multi_set: ok');
  });

  const hasProcessAbiCommands =
    existsSync(join(COMMANDS_DIR, 'bash')) && existsSync(join(COMMANDS_DIR, 'git'));

  itIf(
    hasProcessAbiCommands,
    'process ABI: current git and legacy sh/bash retain spawn/wait compatibility',
    async () => {
      // sh/bash use the older split exec-path/wait-status ABI while the rebuilt
      // Git artifact uses the current ordered-actions/raw-status ABI. Exercise
      // both generations as parent processes.
      const gitHigh = await kernel.exec(`git -c 'alias.legacy=!exit 137' legacy`);
      expect(gitHigh.exitCode,
        `git-high stdout:\n${gitHigh.stdout}\ngit-high stderr:\n${gitHigh.stderr}`).toBe(137);

      const git = await kernel.exec(
        `git -c 'alias.legacy=!f() { test "$1" = argv-ok && test "$LEGACY_ENV" = env-ok && cat; return 23; }; f' legacy argv-ok`,
        { env: { LEGACY_ENV: 'env-ok' }, stdin: 'legacy-stdio' },
      );
      expect(git.exitCode,
        `git stdout:\n${git.stdout}\ngit stderr:\n${git.stderr}`).toBe(23);
      expect(git.stdout).toBe('legacy-stdio');

      const gitSignal = await kernel.exec(
        `git -c 'alias.legacy=!kill -TERM $$' legacy`,
      );
      expect(gitSignal.exitCode,
        `git-signal stdout:\n${gitSignal.stdout}\ngit-signal stderr:\n${gitSignal.stderr}`).toBe(143);

      const sh = await kernel.exec(`sh -c 'exit 137'`);
      expect(sh.exitCode, `sh stdout:\n${sh.stdout}\nsh stderr:\n${sh.stderr}`).toBe(137);

      const bashLow = await kernel.exec(`bash -c 'sh -c "exit 23"; exit $?'`);
      expect(bashLow.exitCode,
        `bash-low stdout:\n${bashLow.stdout}\nbash-low stderr:\n${bashLow.stderr}`).toBe(23);

      const bash = await kernel.exec(`bash -c 'sh -c "exit 137"; exit $?'`);
      expect(bash.exitCode, `bash stdout:\n${bash.stdout}\nbash stderr:\n${bash.stderr}`).toBe(137);
    },
  );

  itIf(!tier3Skip, 'socket_flags: socket and accept4 creation flags and errnos match Linux', async () => {
    const native = await runNative('socket_flags');
    const wasm = await kernel.exec('socket_flags');

    const diagnostic =
      `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}` +
      `\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`;
    expect(wasm.exitCode, diagnostic).toBe(native.exitCode);
    expect(wasm.exitCode, diagnostic).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('socket_nonblock=yes');
    expect(wasm.stdout).toContain('socket_cloexec=yes');
    expect(wasm.stdout).toContain('socket_invalid_flag_einval=yes');
    expect(wasm.stdout).toContain('accept4_badfd_ebadf=yes');
    expect(wasm.stdout).toContain('accept4_invalid_flag_einval=yes');
    expect(wasm.stdout).toContain('accept4_nonblock=yes');
    expect(wasm.stdout).toContain('accept4_cloexec=yes');
    expect(wasm.stdout).toContain('socket_dup_shared_ofd=yes');
    expect(wasm.stdout).toContain('socket_dup2_exact=yes');
    expect(wasm.stdout).toContain('socket_f_dupfd_min=yes');
    expect(wasm.stdout).toContain('socket_dup_independent_close=yes');
    expect(wasm.stdout).toContain('socket_flags=ok');
  });

  itIf(!tier3Skip, 'unix_socket: abstract bind/connect and byte transfer match Linux', async () => {
    const native = await runNative('unix_socket', ['--abstract-contract']);
    const wasm = await kernel.exec('unix_socket --abstract-contract');

    expect(
      wasm.exitCode,
      `native stdout:\n${native.stdout}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toBe('abstract_unix_namespace=ok\n');
  });

  itIf(!tier3Skip, 'unix_socket: partial reads, MSG_PEEK, and blocking wakeups match Linux', async () => {
    const native = await runNative('unix_socket', ['--stream-read-contract']);
    const wasm = await kernel.exec('unix_socket --stream-read-contract');

    expect(
      wasm.exitCode,
      `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toBe('unix_stream_read_contract=ok\n');
  });

  itIf(!tier3Skip, 'unix_socket: sockaddr lengths, pointer ordering, and lifecycle match Linux', async () => {
    const native = await runNative('unix_socket', ['--parity-contract']);
    await vfs.createDir('/tmp');
    const wasm = await kernel.exec('unix_socket --parity-contract');

    expect(
      wasm.exitCode,
      `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('unix_family_only_autobind=yes');
    expect(wasm.stdout).toContain('unix_max_pathname_length=yes');
    expect(wasm.stdout).toContain('accept_null_len_consumes_connection=yes');
    expect(wasm.stdout).toContain('unix_second_bind_einval=yes');
    expect(wasm.stdout).toContain('unix_repeated_listen=yes');
    expect(wasm.stdout).toContain('unix_second_connect_eisconn=yes');
    expect(wasm.stdout).toContain('unix_socket_parity=ok');
  });

  itIf(!tier3Skip, 'unix_socket: stream-only compatibility exception is explicit', async () => {
    const native = await runNative('unix_socket', ['--socket-type-contract']);
    const wasm = await kernel.exec('unix_socket --socket-type-contract');

    expect(native.exitCode, native.stderr).toBe(0);
    expect(wasm.exitCode, wasm.stderr).toBe(0);
    expect(native.stdout).toBe('unix_dgram=supported\nunix_seqpacket=supported\n');
    expect(wasm.stdout).toBe('unix_dgram=unsupported\nunix_seqpacket=unsupported\n');
  });

  itIf(!tier3Skip, 'socketpair_rights: duplex I/O, spawn inheritance, and SCM_RIGHTS match Linux', async () => {
    const native = await runNative('socketpair_rights');
    const wasm = await kernel.exec('socketpair_rights');

    expect(
      wasm.exitCode,
      `native stdout:\n${native.stdout}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('socketpair_nonblock=yes');
    expect(wasm.stdout).toContain('socketpair_cloexec=yes');
    expect(wasm.stdout).toContain('socketpair_poll=yes');
    expect(wasm.stdout).toContain('socketpair_bidirectional=yes');
    expect(wasm.stdout).toContain('socketpair_shutdown_eof=yes');
    expect(wasm.stdout).toContain('socketpair_recvmsg_flags=yes');
    expect(wasm.stdout).toContain('scm_rights_cross_process=yes');
    expect(wasm.stdout).toContain('scm_rights_cloexec=yes');
    expect(wasm.stdout).toContain('scm_rights_pending_tcp_socket=yes');
    expect(wasm.stdout).toContain('scm_rights_tcp_listener_connection=yes');
    expect(wasm.stdout).toContain('scm_rights_hostnet_peek_shared=yes');
    expect(wasm.stdout).toContain('scm_rights_udp_socket=yes');
    expect(wasm.stdout).toContain('socketpair_rights=ok');
  });

  itIf(!tier3Skip, 'exec_edge: replacement preserves argv and closes only CLOEXEC fds', async () => {
    const native = await runNative('exec_edge');
    const wasm = await kernel.exec('exec_edge');

    expect(
      wasm.exitCode,
      `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
    ).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toBe(native.stdout);
    expect(wasm.stdout).toContain('exec_argv0: yes');
    expect(wasm.stdout).toContain('exec_empty_arg: yes');
    expect(wasm.stdout).toContain('exec_keep_fd: yes');
    expect(wasm.stdout).toContain('exec_cloexec_closed: yes');
    expect(wasm.stdout).toContain('exec_fd_io: yes');
    expect(wasm.stdout).toContain('exec_proc_cmdline: yes');
    expect(wasm.stdout).toContain('exec_proc_environ: yes');
    expect(wasm.stdout).toContain('exec_replacement: ok');
  });

  for (const [mode, marker] of [
    ['execle', 'execle: ok'],
    ['execvpe', 'execvpe: ok'],
    ['execve-shebang', 'execve_shebang: ok'],
    ['shell-fallback', 'shell_fallback: ok'],
    ['fexecve', 'fexecve_unlinked_cloexec: ok'],
    ['fexecve-script', 'fexecve_script_unlinked: ok'],
    ['fexecve-script-cloexec', 'fexecve_script_cloexec_enoent: ok'],
  ] as const) {
    itIf(!tier3Skip, `exec_variants ${mode}: matches Linux replacement behavior`, async () => {
      const native = await runNative('exec_variants', [mode]);
      const wasm = await kernel.exec(`exec_variants ${mode}`);

      expect(
        wasm.exitCode,
        `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
      ).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toBe(native.stdout);
      expect(wasm.stdout).toContain(marker);
    });
  }

  // --- Capstone: syscall coverage (all tiers, patched sysroot) ---

  const hasSyscallCoverage = existsSync(join(C_BUILD_DIR, 'syscall_coverage'));
  const syscallCoverageSkip = !hasSyscallCoverage
    ? 'syscall_coverage WASM binary not built (need patched sysroot: make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs)'
    : false;

  itIf(!syscallCoverageSkip, 'syscall_coverage: all syscall categories pass parity', async () => {
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
      'pipe', 'dup', 'dup2', 'closefrom', 'getpid', 'getppid', 'sigaction_register', 'sigaction_query', 'spawn_waitpid', 'kill',
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

  itIf(!tier4Skip, 'c-ls: directory listing with file sizes matches', async () => {
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

  itIf(!tier4Skip, 'c-tree: recursive directory listing matches', async () => {
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

  itIf(!tier4Skip, 'c-find: find files matching glob pattern', async () => {
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

  itIf(!tier4Skip, 'c-cp: copied file contents match', async () => {
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

  itIf(!sqliteSkip, 'sqlite3_mem: in-memory SQL operations parity', async () => {
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

  itIf(!tier5Skip, 'json_parse: cJSON parse and format parity', async () => {
    const sampleJson = JSON.stringify({
      name: 'agentos',
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
    expect(wasm.stdout).toContain('"name": "agentos"');
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

  itIf(!netSkip, 'tcp_echo: connect to TCP echo server, send and receive', async () => {
    // Start a local TCP echo server
    const server = createTcpServer((conn) => {
      conn.on('data', (data) => { conn.write(data); conn.end(); });
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const port = (server.address() as import('node:net').AddressInfo).port;

    try {
      await recreateKernel({ loopbackExemptPorts: [port] });
      const native = await runNative('tcp_echo', [String(port)]);
      const wasm = await kernel.exec(`tcp_echo ${port}`);

      expect(
        wasm.exitCode,
        `native stdout:\n${native.stdout}\nnative stderr:\n${native.stderr}\nWASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`,
      ).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      expect(wasm.stdout).toBe(native.stdout);
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toContain('sent: 5');
      expect(wasm.stdout).toContain('received: hello');
    } finally {
      server.close();
    }
  });

  itIf(!netSkip, 'curl: connect to HTTP server, receive response body', async () => {
    // Start a local HTTP server
    const server = createHttpServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      res.end('hello from http');
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const port = (server.address() as import('node:net').AddressInfo).port;

    try {
      await recreateKernel({ loopbackExemptPorts: [port] });
      const wasm = await kernel.exec(`curl -fsS http://127.0.0.1:${port}/`);

      expect(wasm.exitCode).toBe(0);
      expect(wasm.stderr).toBe('');
      expect(wasm.stdout.trim()).toBe('hello from http');
    } finally {
      server.close();
    }
  });

  itIf(!netSkip, 'dns_lookup: resolve localhost to 127.0.0.1', async () => {
    const native = await runNative('dns_lookup', ['localhost']);
    const wasm = await kernel.exec('dns_lookup localhost');

    expect(wasm.exitCode).toBe(native.exitCode);
    expect(wasm.exitCode).toBe(0);
    expect(wasm.stdout).toBe(native.stdout);
    expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
    expect(wasm.stdout).toContain('host: localhost');
    expect(wasm.stdout).toContain('ip: 127.0.0.1');
  });

  const hasGetaddrinfoConnect =
    existsSync(join(C_BUILD_DIR, 'getaddrinfo_connect')) &&
    existsSync(join(NATIVE_DIR, 'getaddrinfo_connect'));
  itIf(
    hasGetaddrinfoConnect,
    'getaddrinfo: live hosts alias and services name connect like Linux',
    async () => {
      const server = createTcpServer((socket) => socket.end());
      await new Promise<void>((resolveListen) => server.listen(0, '127.0.0.1', resolveListen));
      const port = (server.address() as import('node:net').AddressInfo).port;
      await recreateKernel({ loopbackExemptPorts: [port] });
      const hosts = '127.0.0.1 servicebox service-alias\n';
      const services = `agentos-parity ${port}/tcp\n`;
      await vfs.writeFile('/etc/hosts', hosts);
      await vfs.writeFile('/etc/services', services);
      try {
        const wasm = await kernel.exec('getaddrinfo_connect service-alias agentos-parity');
        expect(wasm.exitCode, wasm.stderr).toBe(0);
        expect(wasm.stdout).toBe('getaddrinfo_live_hosts_service_connect=yes\n');

        const nativeDir = await mkdtemp(join(tmpdir(), 'agentos-netdb-'));
        const nativeHosts = join(nativeDir, 'hosts');
        const nativeServices = join(nativeDir, 'services');
        await fsWriteFile(nativeHosts, hosts);
        await fsWriteFile(nativeServices, services);
        const probe = spawnSync('unshare', [
          '-Urm', 'sh', '-c',
          'mount --bind "$1" /etc/hosts && mount --bind "$2" /etc/services',
          'sh', nativeHosts, nativeServices,
        ], { stdio: 'ignore' });
        if (probe.status === 0) {
          const native = await runNativeWithNetworkFiles(
            'getaddrinfo_connect', nativeHosts, nativeServices,
            ['service-alias', 'agentos-parity'],
          );
          expect(native).toEqual(wasm);
        }
        await rm(nativeDir, { recursive: true, force: true });
      } finally {
        server.close();
      }
    },
  );

  const hasGetnameinfoContract =
    existsSync(join(C_BUILD_DIR, 'getnameinfo_contract')) &&
    existsSync(join(NATIVE_DIR, 'getnameinfo_contract'));
  itIf(
    hasGetnameinfoContract,
    'getnameinfo_contract: Linux flags, PTR fallback, scopes, and services database match',
    async () => {
	  const hosts = [
		'127.0.0.1 localhost localhost.localdomain',
		'::1 localhost localhost.localdomain ip6-localhost ip6-loopback',
		'203.0.113.9 custombox.example.test custombox aliases',
		'fe80::1234%lo scopedbox',
		'',
	  ].join('\n');
      await vfs.writeFile(
        '/etc/services',
        [
          'ssh 22/tcp',
          'smtp 25/tcp mail',
          'shell 514/tcp cmd',
          'syslog 514/udp',
          '',
        ].join('\n'),
      );
	  await vfs.writeFile('/etc/hosts', hosts);
	  const nativeHostsDir = await mkdtemp(join(tmpdir(), 'agentos-hosts-'));
	  const nativeHostsFile = join(nativeHostsDir, 'hosts');
	  await fsWriteFile(nativeHostsFile, hosts);
	  const mountProbe = spawnSync('unshare', [
		'-Urm',
		'sh',
		'-c',
		'mount --bind "$1" /etc/hosts',
		'sh',
		nativeHostsFile,
	  ], { stdio: 'ignore' });
	  const native = mountProbe.status === 0
		? await runNativeWithHosts('getnameinfo_contract', nativeHostsFile)
		: await runNative('getnameinfo_contract', ['--common']);
	  const wasm = await kernel.exec('getnameinfo_contract');
	  await rm(nativeHostsDir, { recursive: true, force: true });

      expect(wasm.exitCode, `WASM stdout:\n${wasm.stdout}\nWASM stderr:\n${wasm.stderr}`).toBe(native.exitCode);
      expect(wasm.exitCode).toBe(0);
      const targetSpecific = (output: string) => output
        .split('\n')
        .filter((line) =>
          !line.includes('musl_accepts_numericscope_flag') &&
          !line.includes('glibc_rejects_numericscope_flag') &&
          !line.includes('ipv6_forced_numeric_scope') &&
          !line.includes('mapped_ipv6_live_hosts') &&
		  !line.includes('live_hosts_precedes_dns') &&
		  !line.includes('musl_nofqdn_noop') &&
		  !line.includes('scoped_hosts_') &&
		  !line.includes('forward_scoped_hosts') &&
		  !line.includes('forward_hosts_alias_service_canonname') &&
          !line.includes('getnameinfo_contract='),
        )
        .join('\n');
      expect(targetSpecific(wasm.stdout)).toBe(targetSpecific(native.stdout));
      expect(normalizeStderr(wasm.stderr)).toBe(normalizeStderr(native.stderr));
      expect(wasm.stdout).toContain('getnameinfo_ptr_miss_prefilled_numeric_fallback=yes');
      expect(wasm.stdout).toContain('getnameinfo_services_database=yes');
	  expect(wasm.stdout).toContain('getnameinfo_live_hosts_precedes_dns=yes');
	  expect(wasm.stdout).toContain('getnameinfo_musl_nofqdn_noop=yes');
	  expect(wasm.stdout).toContain('getnameinfo_mapped_ipv6_live_hosts=yes');
	  expect(wasm.stdout).toContain('getnameinfo_musl_accepts_numericscope_flag=yes');
	  expect(wasm.stdout).toContain('getnameinfo_ipv6_named_scope=yes');
	  expect(wasm.stdout).toContain('getnameinfo_ipv6_forced_numeric_scope=yes');
	  expect(wasm.stdout).toContain('getnameinfo_ipv6_unknown_scope_numeric_fallback=yes');
	  expect(wasm.stdout).toContain('getnameinfo_ipv6_global_scope_stays_numeric=yes');
	  expect(wasm.stdout).toContain('getnameinfo_loopback_name_to_index=yes');
	  expect(wasm.stdout).toContain('getnameinfo_loopback_index_to_name=yes');
	  expect(wasm.stdout).toContain('getnameinfo_loopback_getifaddrs=yes');
	  expect(wasm.stdout).toContain('getnameinfo_loopback_nameindex_list=yes');
	  expect(wasm.stdout).toContain('getnameinfo_scoped_hosts_match=yes');
	  expect(wasm.stdout).toContain('getnameinfo_scoped_hosts_mismatch=yes');
	  expect(wasm.stdout).toContain('getnameinfo_forward_hosts_alias_service_canonname=yes');
	  expect(wasm.stdout).toContain('getnameinfo_forward_scoped_hosts=yes');
      expect(wasm.stdout).toContain('getnameinfo_contract=ok');
    },
  );
});
