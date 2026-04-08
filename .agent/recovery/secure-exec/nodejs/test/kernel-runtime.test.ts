/**
 * Tests for the Node.js RuntimeDriver.
 *
 * Verifies driver interface contract, kernel mounting, command
 * registration, KernelCommandExecutor routing, and isolate-based
 * script execution.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createNodeRuntime } from '../src/kernel-runtime.ts';
import type { NodeRuntimeOptions } from '../src/kernel-runtime.ts';
import { createKernel } from '@secure-exec/core';
import type {
  KernelRuntimeDriver as RuntimeDriver,
  KernelInterface,
  ProcessContext,
  DriverProcess,
  Kernel,
} from '@secure-exec/core';

/**
 * Minimal mock RuntimeDriver for testing cross-runtime dispatch.
 * Configurable per-command exit codes and stdout/stderr output.
 */
class MockRuntimeDriver implements RuntimeDriver {
  name = 'mock';
  commands: string[];
  private _configs: Record<string, { exitCode?: number; stdout?: string; stderr?: string }>;

  constructor(commands: string[], configs: Record<string, { exitCode?: number; stdout?: string; stderr?: string }> = {}) {
    this.commands = commands;
    this._configs = configs;
  }

  async init(_kernel: KernelInterface): Promise<void> {}

  spawn(command: string, args: string[], ctx: ProcessContext): DriverProcess {
    // Handle bash -c 'cmd ...' by extracting the inner command name
    let resolvedCmd = command;
    if ((command === 'bash' || command === 'sh') && args[0] === '-c' && args[1]) {
      resolvedCmd = args[1].split(/\s+/)[0];
    }
    const config = this._configs[resolvedCmd] ?? {};
    const exitCode = config.exitCode ?? 0;

    let resolveExit!: (code: number) => void;
    const exitPromise = new Promise<number>((r) => { resolveExit = r; });

    const proc: DriverProcess = {
      onStdout: null,
      onStderr: null,
      onExit: null,
      writeStdin: () => {},
      closeStdin: () => {},
      kill: () => {},
      wait: () => exitPromise,
    };

    // Emit output asynchronously
    queueMicrotask(() => {
      if (config.stdout) {
        const data = new TextEncoder().encode(config.stdout);
        ctx.onStdout?.(data);
        proc.onStdout?.(data);
      }
      if (config.stderr) {
        const data = new TextEncoder().encode(config.stderr);
        ctx.onStderr?.(data);
        proc.onStderr?.(data);
      }
      resolveExit(exitCode);
      proc.onExit?.(exitCode);
    });

    return proc;
  }

  async dispose(): Promise<void> {}
}

// Minimal in-memory VFS for kernel tests
class SimpleVFS {
  private files = new Map<string, Uint8Array>();
  private dirs = new Set<string>(['/']);

  async readFile(path: string): Promise<Uint8Array> {
    const data = this.files.get(path);
    if (!data) throw new Error(`ENOENT: ${path}`);
    return data;
  }
  async readTextFile(path: string): Promise<string> {
    return new TextDecoder().decode(await this.readFile(path));
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
    return (await this.readDir(path)).map(name => ({
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
  async mkdir(path: string) { this.dirs.add(path); }
  async exists(path: string): Promise<boolean> {
    return this.files.has(path) || this.dirs.has(path);
  }
  async stat(path: string) {
    const isDir = this.dirs.has(path);
    const data = this.files.get(path);
    if (!isDir && !data) throw new Error(`ENOENT: ${path}`);
    return {
      mode: isDir ? 0o40755 : 0o100644,
      size: data?.length ?? 0,
      isDirectory: isDir,
      isSymbolicLink: false,
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
  async removeFile(path: string) { this.files.delete(path); }
  async removeDir(path: string) { this.dirs.delete(path); }
  async rename(oldPath: string, newPath: string) {
    const data = this.files.get(oldPath);
    if (data) { this.files.set(newPath, data); this.files.delete(oldPath); }
  }
  async realpath(path: string) { return path; }
  async symlink(_target: string, _linkPath: string) {}
  async readlink(_path: string): Promise<string> { return ''; }
  async lstat(path: string) { return this.stat(path); }
  async link(_old: string, _new: string) {}
  async chmod(_path: string, _mode: number) {}
  async chown(_path: string, _uid: number, _gid: number) {}
  async utimes(_path: string, _atime: number, _mtime: number) {}
  async truncate(_path: string, _length: number) {}
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

describe('Node RuntimeDriver', () => {
  describe('factory', () => {
    it('createNodeRuntime returns a RuntimeDriver', () => {
      const driver = createNodeRuntime();
      expect(driver).toBeDefined();
      expect(driver.name).toBe('node');
      expect(typeof driver.init).toBe('function');
      expect(typeof driver.spawn).toBe('function');
      expect(typeof driver.dispose).toBe('function');
    });

    it('driver.name is "node"', () => {
      const driver = createNodeRuntime();
      expect(driver.name).toBe('node');
    });

    it('driver.commands contains node, npm, npx', () => {
      const driver = createNodeRuntime();
      expect(driver.commands).toContain('node');
      expect(driver.commands).toContain('npm');
      expect(driver.commands).toContain('npx');
    });

    it('accepts custom memoryLimit', () => {
      // Verify option is stored and differs from default (128)
      const driver = createNodeRuntime({ memoryLimit: 256 });
      expect((driver as any)._memoryLimit).toBe(256);
    });

    it('memoryLimit defaults to 128', () => {
      const driver = createNodeRuntime();
      expect((driver as any)._memoryLimit).toBe(128);
    });
  });

  describe('driver lifecycle', () => {
    it('throws when spawning before init', () => {
      const driver = createNodeRuntime();
      const ctx: ProcessContext = {
        pid: 1, ppid: 0, env: {}, cwd: '/home/user',
        fds: { stdin: 0, stdout: 1, stderr: 2 },
      };
      expect(() => driver.spawn('node', ['-e', 'true'], ctx)).toThrow(/not initialized/);
    });

    it('dispose without init does not throw', async () => {
      const driver = createNodeRuntime();
      await driver.dispose();
    });

    it('dispose after init cleans up', async () => {
      const driver = createNodeRuntime();
      const mockKernel: Partial<KernelInterface> = {};
      await driver.init(mockKernel as KernelInterface);
      await driver.dispose();
    });
  });

  describe('kernel integration', () => {
    let kernel: Kernel;

    afterEach(async () => {
      await kernel?.dispose();
    });

    it('mounts to kernel successfully', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      const driver = createNodeRuntime();
      await kernel.mount(driver);

      expect(kernel.commands.get('node')).toBe('node');
      expect(kernel.commands.get('npm')).toBe('node');
      expect(kernel.commands.get('npx')).toBe('node');
    });

    it('node -e executes inline code and exits 0', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const proc = kernel.spawn('node', ['-e', 'console.log("hello from node")']);
      const stdoutChunks: string[] = [];
      // Collect stdout via wait — process completes and exec captures it
      const code = await proc.wait();
      expect(code).toBe(0);
    });

    it('node -e captures stdout', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', 'console.log("hello")'], {
        onStdout: (data) => chunks.push(data),
      });
      await proc.wait();

      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(output).toContain('hello');
    });

    it('node -e with error exits non-zero', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const proc = kernel.spawn('node', ['-e', 'throw new Error("boom")']);
      const code = await proc.wait();
      expect(code).not.toBe(0);
    });

    it('node script reads from VFS', async () => {
      const vfs = new SimpleVFS();
      await vfs.writeFile('/app/hello.js', 'console.log("from vfs")');
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['/app/hello.js'], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      expect(code).toBe(0);

      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(output).toContain('from vfs');
    });

    it('node script with missing file exits non-zero', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const errChunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['/nonexistent.js'], {
        onStderr: (data) => errChunks.push(data),
      });
      const code = await proc.wait();
      expect(code).not.toBe(0);

      const stderr = errChunks.map(c => new TextDecoder().decode(c)).join('');
      expect(stderr).toContain('Cannot find module');
    });

    it('node -p evaluates expression', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-p', '1 + 2'], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      expect(code).toBe(0);

      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(output).toContain('3');
    });

    it('node with no args exits non-zero', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const proc = kernel.spawn('node', []);
      const code = await proc.wait();
      expect(code).not.toBe(0);
    });

    it('dispose cleans up active isolates', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      const driver = createNodeRuntime();
      await kernel.mount(driver);

      await kernel.dispose();
      // Double dispose is safe
      await kernel.dispose();
    });
  });

  describe('KernelCommandExecutor routing', () => {
    let kernel: Kernel;

    afterEach(async () => {
      await kernel?.dispose();
    });

    it('child_process.spawnSync routes through kernel — spy driver records call', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });

      // Spy driver records every spawn call for later assertion
      const spy = { calls: [] as { command: string; args: string[]; callerPid: number }[] };
      const spyDriver = new MockRuntimeDriver(['echo'], {
        echo: { exitCode: 0, stdout: 'spy-echo-output' },
      });
      const originalSpawn = spyDriver.spawn.bind(spyDriver);
      spyDriver.spawn = (command: string, args: string[], ctx: ProcessContext): DriverProcess => {
        spy.calls.push({ command, args: [...args], callerPid: ctx.ppid });
        return originalSpawn(command, args, ctx);
      };

      await kernel.mount(spyDriver);
      await kernel.mount(createNodeRuntime());

      // spawnSync passes command and args directly (no bash -c wrapping)
      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        const { spawnSync } = require('child_process');
        const result = spawnSync('echo', ['hello']);
        console.log('child output:', result.stdout.toString().trim());
      `], {
        onStdout: (data) => chunks.push(data),
      });

      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');

      // Spy proves routing happened — not just that output appeared
      expect(spy.calls.length).toBe(1);
      expect(spy.calls[0].command).toBe('echo');
      expect(spy.calls[0].args).toEqual(['hello']);
      expect(spy.calls[0].callerPid).toBeGreaterThan(0);
      expect(code).toBe(0);
      expect(output).toContain('spy-echo-output');
    });

    it('child_process stdout works with readline consumers', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        const { spawn } = require('child_process');
        const readline = require('readline');

        const child = spawn('node', ['-e', 'console.log("child-line")']);
        const rl = readline.createInterface({ input: child.stdout });

        rl.on('line', (line) => {
          console.log('line:' + line);
        });

        child.on('exit', (code) => {
          console.log('exit:' + code);
        });
      `], {
        onStdout: (data) => chunks.push(data),
      });

      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');

      expect(code).toBe(0);
      expect(output).toContain('line:child-line');
      expect(output).toContain('exit:0');
    });
  });

  describe('stdin streaming', () => {
    let kernel: Kernel;

    afterEach(async () => {
      await kernel?.dispose();
    });

    it('writeStdin delivers data to Node process', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        let d = '';
        process.stdin.on('data', c => d += c);
        process.stdin.on('end', () => console.log(d.trim()));
      `], {
        onStdout: (data) => chunks.push(data),
      });

      proc.writeStdin(new TextEncoder().encode('hello from stdin'));
      proc.closeStdin();

      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      expect(output).toContain('hello from stdin');
    });

    it('closeStdin without write triggers empty EOF', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        const data = process.stdin.read();
        console.log(data === null ? 0 : data.length);
      `], {
        onStdout: (data) => chunks.push(data),
      });

      proc.closeStdin();

      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      expect(output).toContain('0');
    });

    it('streamStdin writeStdin delivers data exactly once per write', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      // Script: count every stdin data event and log each chunk to stderr
      const stderrChunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        let count = 0;
        process.stdin.on('data', (chunk) => {
          count++;
          const text = typeof chunk === 'string' ? chunk : new TextDecoder().decode(chunk);
          process.stderr.write('CHUNK:' + count + ':' + text.trim() + '\\n');
        });
        process.stdin.on('end', () => {
          process.stderr.write('TOTAL:' + count + '\\n');
        });
      `], {
        streamStdin: true,
        onStderr: (data) => stderrChunks.push(data),
      });

      // Write 3 messages with small delays between them
      const enc = new TextEncoder();
      proc.writeStdin(enc.encode('msg1\n'));
      await new Promise(r => setTimeout(r, 100));
      proc.writeStdin(enc.encode('msg2\n'));
      await new Promise(r => setTimeout(r, 100));
      proc.writeStdin(enc.encode('msg3\n'));
      await new Promise(r => setTimeout(r, 100));
      proc.closeStdin();

      const code = await proc.wait();
      const stderr = stderrChunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);

      // Each message must arrive exactly once — no doubling
      const chunkLines = stderr.split('\n').filter(l => l.startsWith('CHUNK:'));
      expect(chunkLines).toHaveLength(3);
      expect(chunkLines[0]).toContain('msg1');
      expect(chunkLines[1]).toContain('msg2');
      expect(chunkLines[2]).toContain('msg3');

      // Total must be exactly 3
      expect(stderr).toContain('TOTAL:3');
    });

    it('concurrent streamStdin processes receive data independently', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const stderrA: Uint8Array[] = [];
      const stderrB: Uint8Array[] = [];

      // Spawn two echo-stdin processes with streamStdin
      const procA = kernel.spawn('node', ['-e', `
        process.stdin.on('data', (d) => {
          const text = typeof d === 'string' ? d : new TextDecoder().decode(d);
          process.stderr.write('A:' + text.trim() + '\\n');
        });
      `], {
        streamStdin: true,
        onStderr: (data) => stderrA.push(data),
      });

      const procB = kernel.spawn('node', ['-e', `
        process.stdin.on('data', (d) => {
          const text = typeof d === 'string' ? d : new TextDecoder().decode(d);
          process.stderr.write('B:' + text.trim() + '\\n');
        });
      `], {
        streamStdin: true,
        onStderr: (data) => stderrB.push(data),
      });

      // Wait for both to start
      await new Promise(r => setTimeout(r, 500));

      // Write to each process
      const enc = new TextEncoder();
      procA.writeStdin(enc.encode('hello-A\n'));
      procB.writeStdin(enc.encode('hello-B\n'));

      await new Promise(r => setTimeout(r, 500));

      const outA = stderrA.map(c => new TextDecoder().decode(c)).join('');
      const outB = stderrB.map(c => new TextDecoder().decode(c)).join('');

      // Each process must receive only its own data
      expect(outA).toContain('A:hello-A');
      expect(outB).toContain('B:hello-B');
      // Data must not cross between processes
      expect(outA).not.toContain('hello-B');
      expect(outB).not.toContain('hello-A');

      procA.kill();
      procB.kill();
    });
  });

  describe('exploit/abuse paths', () => {
    let kernel: Kernel;

    afterEach(async () => {
      await kernel?.dispose();
    });

    it('cannot escape isolate via process.exit', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const proc = kernel.spawn('node', ['-e', 'process.exit(42)']);
      const code = await proc.wait();
      // process.exit should not crash the host — just exit the isolate
      expect(typeof code).toBe('number');
      expect(code).not.toBe(0);
    });

    it('fs.readFileSync /etc/passwd returns error', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        const fs = require('fs');
        try {
          fs.readFileSync('/etc/passwd', 'utf8');
          console.log('FAIL:no-error');
        } catch (e) {
          console.log('code:' + e.code);
        }
      `], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      // Deny-by-default permissions block host path access
      expect(output).toContain('code:EACCES');
      expect(output).not.toContain('FAIL:no-error');
    });

    it('symlink traversal to /etc/passwd returns error', async () => {
      const vfs = new SimpleVFS();
      await vfs.createDir('/tmp');
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        const fs = require('fs');
        try { fs.symlinkSync('/etc/passwd', '/tmp/escape'); } catch (e) {}
        try {
          fs.readFileSync('/tmp/escape', 'utf8');
          console.log('FAIL:no-error');
        } catch (e) {
          console.log('code:' + e.code);
        }
      `], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      // Symlink to host path is blocked by permissions
      expect(output).toContain('code:EACCES');
      expect(output).not.toContain('FAIL:no-error');
    });

    it('relative path traversal ../../etc/passwd returns error', async () => {
      const vfs = new SimpleVFS();
      await vfs.createDir('/app');
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        const fs = require('fs');
        try {
          fs.readFileSync('../../etc/passwd', 'utf8');
          console.log('FAIL:no-error');
        } catch (e) {
          console.log('code:' + e.code);
        }
      `], {
        onStdout: (data) => chunks.push(data),
        cwd: '/app',
      });
      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      // Relative traversal to host path is blocked by permissions
      expect(output).toContain('code:EACCES');
      expect(output).not.toContain('FAIL:no-error');
    });

    it('concurrent child process spawning assigns unique PIDs', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });

      // Spy driver records child PIDs assigned by the kernel
      const childPids: number[] = [];
      const spyDriver = new MockRuntimeDriver(['echo'], {
        echo: { exitCode: 0, stdout: 'ok' },
      });
      const originalSpawn = spyDriver.spawn.bind(spyDriver);
      spyDriver.spawn = (command: string, args: string[], ctx: ProcessContext): DriverProcess => {
        childPids.push(ctx.pid);
        return originalSpawn(command, args, ctx);
      };

      await kernel.mount(spyDriver);
      await kernel.mount(createNodeRuntime());

      // Spawn 12 child processes via spawnSync — each routed through kernel
      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        const { spawnSync } = require('child_process');
        const results = [];
        for (let i = 0; i < 12; i++) {
          const r = spawnSync('echo', [String(i)]);
          results.push(r.status === 0 ? 'ok' : 'err');
        }
        console.log(results.join(','));
      `], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');

      expect(code).toBe(0);
      expect(output).toContain('ok,ok,ok,ok,ok,ok,ok,ok,ok,ok,ok,ok');

      // Spy proves each child got a unique PID from kernel process table
      expect(childPids.length).toBe(12);
      const uniquePids = new Set(childPids);
      expect(uniquePids.size).toBe(12);
    });
  });

  describe('CJS event loop pumping', () => {
    let kernel: Kernel;

    afterEach(async () => {
      await kernel?.dispose();
    });

    it('setTimeout callback fires before CJS script exits', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        setTimeout(() => {
          console.log("TIMER_FIRED");
          process.exit(0);
        }, 100);
      `], {
        onStdout: (data) => chunks.push(data),
      });

      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      expect(output).toContain('TIMER_FIRED');
    });

    it('async main with console.log produces output', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        async function main() {
          console.log("ASYNC_HELLO");
        }
        main();
      `], {
        onStdout: (data) => chunks.push(data),
      });

      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      expect(output).toContain('ASYNC_HELLO');
    });

    it('chained setTimeout callbacks all execute', async () => {
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime());

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('node', ['-e', `
        let count = 0;
        function step() {
          count++;
          console.log("STEP:" + count);
          if (count < 3) {
            setTimeout(step, 50);
          } else {
            process.exit(0);
          }
        }
        setTimeout(step, 50);
      `], {
        onStdout: (data) => chunks.push(data),
      });

      const code = await proc.wait();
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(code).toBe(0);
      expect(output).toContain('STEP:1');
      expect(output).toContain('STEP:2');
      expect(output).toContain('STEP:3');
    });
  });

  describe('bare command resolution from node_modules/.bin', () => {
    let kernel: Kernel;
    let tmpDir: string;

    function createMockBinDir() {
      tmpDir = join(tmpdir(), `se-bin-test-${Date.now()}`);
      const binDir = join(tmpDir, 'node_modules', '.bin');
      const pkgDir = join(tmpDir, 'node_modules', 'my-tool', 'dist');
      mkdirSync(binDir, { recursive: true });
      mkdirSync(pkgDir, { recursive: true });
      // Create a real JS entry file
      writeFileSync(
        join(pkgDir, 'cli.js'),
        'console.log("hello from bare command");',
      );
      // Create a pnpm-style shell wrapper in .bin
      writeFileSync(
        join(binDir, 'my-tool'),
        [
          '#!/bin/sh',
          'basedir=$(dirname "$(echo "$0" | sed -e \'s,\\\\,/,g\')")',
          'exec node  "$basedir/../my-tool/dist/cli.js" "$@"',
        ].join('\n'),
        { mode: 0o755 },
      );
      return tmpDir;
    }

    afterEach(async () => {
      await kernel?.dispose();
      if (tmpDir) {
        try { rmSync(tmpDir, { recursive: true, force: true }); } catch {}
      }
    });

    it('tryResolve returns true for bare command in node_modules/.bin', () => {
      createMockBinDir();
      const driver = createNodeRuntime({ moduleAccessCwd: tmpDir });
      expect(driver.tryResolve!('my-tool')).toBe(true);
    });

    it('tryResolve returns false for unknown bare command', () => {
      createMockBinDir();
      const driver = createNodeRuntime({ moduleAccessCwd: tmpDir });
      expect(driver.tryResolve!('nonexistent-tool')).toBe(false);
    });

    it('tryResolve returns false when moduleAccessCwd is not set', () => {
      const driver = createNodeRuntime();
      expect(driver.tryResolve!('my-tool')).toBe(false);
    });

    it('bare command executes the resolved JS entry point', async () => {
      createMockBinDir();
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime({ moduleAccessCwd: tmpDir }));

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('my-tool', [], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      expect(code).toBe(0);

      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(output).toContain('hello from bare command');
    });

    it('bare command runs successfully even when args are passed', async () => {
      createMockBinDir();
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime({ moduleAccessCwd: tmpDir }));

      // Spawn with extra args — should not crash
      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('my-tool', ['--flag', 'value'], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      expect(code).toBe(0);

      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(output).toContain('hello from bare command');
    });

    it('handles direct node shebang scripts (npm/yarn symlink style)', async () => {
      createMockBinDir();
      // Replace the shell wrapper with a direct node script
      writeFileSync(
        join(tmpDir, 'node_modules', '.bin', 'my-tool'),
        '#!/usr/bin/env node\nconsole.log("direct node script");',
        { mode: 0o755 },
      );
      const vfs = new SimpleVFS();
      kernel = createKernel({ filesystem: vfs as any });
      await kernel.mount(createNodeRuntime({ moduleAccessCwd: tmpDir }));

      const chunks: Uint8Array[] = [];
      const proc = kernel.spawn('my-tool', [], {
        onStdout: (data) => chunks.push(data),
      });
      const code = await proc.wait();
      expect(code).toBe(0);

      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(output).toContain('direct node script');
    });
  });

  describe('dispose cleanup (no dangling handles)', () => {
    it('kernel.dispose after killing a streamStdin process leaves no active handles', async () => {
      const k = createKernel({ filesystem: new SimpleVFS() });
      await k.mount(createNodeRuntime());

      // Write a long-running stdin reader script
      await k.writeFile('/tmp/reader.mjs', new TextEncoder().encode(
        `process.stdin.setEncoding('utf8');\n` +
        `process.stdin.on('data', (d) => process.stdout.write('GOT:' + d));\n`
      ));

      const chunks: Uint8Array[] = [];
      const proc = k.spawn('node', ['/tmp/reader.mjs'], {
        streamStdin: true,
        onStdout: (data) => chunks.push(data),
      });

      // Send data and verify it's received
      proc.writeStdin('hello\n');
      await new Promise(r => setTimeout(r, 200));
      const output = chunks.map(c => new TextDecoder().decode(c)).join('');
      expect(output).toContain('GOT:hello');

      // Kill the process
      proc.kill();
      const code = await proc.wait();
      expect(code).toBe(143); // SIGTERM = 128 + 15

      // Dispose the kernel — if the IPC socket is still ref'd, vitest hangs
      await k.dispose();
    }, 10_000);

    it('kernel.dispose after killing a streamStdin process closes stdin source', async () => {
      const k = createKernel({ filesystem: new SimpleVFS() });
      await k.mount(createNodeRuntime());

      // Write a script that blocks on stdin
      await k.writeFile('/tmp/blocker.mjs', new TextEncoder().encode(
        `process.stdin.resume();\n` +
        `process.stdin.on('data', () => {});\n`
      ));

      const proc = k.spawn('node', ['/tmp/blocker.mjs'], {
        streamStdin: true,
      });

      // Kill immediately — stdin source should be closed
      proc.kill();
      const code = await proc.wait();
      expect(code).toBe(143);

      await k.dispose();
      // Test passes if it completes without hanging
    }, 10_000);
  });
});
