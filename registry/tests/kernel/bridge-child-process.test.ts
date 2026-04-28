/**
 * Integration tests: Node bridge child_process routing through kernel.
 *
 * Verifies that child_process.spawn/execSync/spawnSync calls from Node
 * isolate code route through the kernel's command registry to the
 * appropriate runtime driver (WasmVM for shell commands).
 *
 * Gracefully skipped when the WASM binary is not built.
 */

import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, it, expect, afterEach } from 'vitest';
import {
  COMMANDS_DIR,
  createKernel,
  createNodeRuntime,
  createWasmVmRuntime,
  describeIf,
  createIntegrationKernel,
  NodeFileSystem,
  skipUnlessWasmBuilt,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const skipReason = skipUnlessWasmBuilt();

describeIf(!skipReason, 'bridge child_process → kernel routing', () => {
  let ctx: IntegrationKernelResult;
  const cleanupPaths: string[] = [];

  afterEach(async () => {
    if (ctx) await ctx.dispose();
    for (const cleanupPath of cleanupPaths.splice(0)) {
      rmSync(cleanupPath, { recursive: true, force: true });
    }
  });

  it('execSync("echo hello") routes through kernel to WasmVM shell', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      const result = execSync('echo hello', { encoding: 'utf-8' });
      console.log(result.trim());
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('hello');
  });

  it('child_process.spawn("ls") resolves to WasmVM runtime', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    await ctx.vfs.writeFile('/tmp/test-file.txt', 'content');

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      const result = execSync('ls /tmp', { encoding: 'utf-8' });
      console.log(result.trim());
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('test-file.txt');
  });

  it('spawned processes get proper PIDs from kernel process table', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // The Node process itself gets a PID from the kernel
    const proc = ctx.kernel.spawn('node', ['-e', 'console.log("pid-test")']);
    expect(proc.pid).toBeGreaterThan(0);

    await proc.wait();
  });

  it('stdout from spawned child processes pipes back to Node caller', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      const result = execSync('echo "piped-output"', { encoding: 'utf-8' });
      console.log('received:', result.trim());
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('received: piped-output');
  });

  it('async child_process.spawn("sh") can stream output and exit cleanly', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { spawn } = require('child_process');
      const child = spawn('sh', ['-lc', 'echo async-ok'], {
        stdio: ['ignore', 'pipe', 'inherit'],
      });
      child.stdout.on('data', (chunk) => process.stdout.write(chunk));
      child.on('close', (code) => process.exit(code ?? 0));
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('async-ok');
  });

  it('child_process.spawn with shell:true preserves shell builtin exit codes', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { spawn } = require('child_process');
      const child = spawn('exit 7', { shell: true });
      child.on('close', (code) => {
        console.log('close:' + code);
        process.exit(code ?? 0);
      });
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(7);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('close:7');
  });

  it('stderr from spawned child processes pipes back to Node caller', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      try {
        execSync('cat /nonexistent/path', { encoding: 'utf-8' });
      } catch (e) {
        console.log('caught-error');
      }
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('caught-error');
  });

  it('commands not in the registry return ENOENT-like error', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      try {
        execSync('nonexistent-cmd-xyz', { encoding: 'utf-8' });
        console.log('SHOULD_NOT_REACH');
      } catch (e) {
        console.log('error-caught');
      }
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    // execSync wraps the command in bash -c, so the shell handles unknown commands
    // Either the shell returns non-zero (caught by execSync) or ENOENT propagates
    expect(output).not.toContain('SHOULD_NOT_REACH');
    expect(output).toContain('error-caught');
  });

  it('execSync with env passes environment through kernel', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      const result = execSync('echo $TEST_VAR', {
        encoding: 'utf-8',
        env: { TEST_VAR: 'kernel-env-test' },
      });
      console.log(result.trim());
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('kernel-env-test');
  });

  it('cat reads VFS file through kernel child_process', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    await ctx.vfs.writeFile('/tmp/bridge-test.txt', 'hello from vfs');

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      const result = execSync('cat /tmp/bridge-test.txt', { encoding: 'utf-8' });
      console.log(result.trim());
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('hello from vfs');
  });

  it('execFileSync on node_modules/.bin shell shims unwraps to the node entrypoint', async () => {
    const projectRoot = mkdtempSync(join(tmpdir(), 'agent-os-node-bin-shim-'));
    cleanupPaths.push(projectRoot);

    mkdirSync(join(projectRoot, 'node_modules', '.bin'), { recursive: true });
    mkdirSync(join(projectRoot, 'node_modules', 'demo'), { recursive: true });
    writeFileSync(
      join(projectRoot, 'node_modules', 'demo', 'index.js'),
      '#!/usr/bin/env node\nconsole.log(JSON.stringify(process.argv.slice(2)));\n',
    );
    chmodSync(join(projectRoot, 'node_modules', 'demo', 'index.js'), 0o755);
    writeFileSync(
      join(projectRoot, 'node_modules', '.bin', 'demo'),
      [
        '#!/bin/sh',
        'basedir=$(dirname "$0")',
        'if [ -x "$basedir/node" ]; then',
        '  exec "$basedir/node" "$basedir/../demo/index.js" "$@"',
        'else',
        '  exec node "$basedir/../demo/index.js" "$@"',
        'fi',
        '',
      ].join('\n'),
    );
    chmodSync(join(projectRoot, 'node_modules', '.bin', 'demo'), 0o755);

    const kernel = createKernel({
      filesystem: new NodeFileSystem({ root: projectRoot }),
    });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
    await kernel.mount(createNodeRuntime());
    ctx = {
      kernel,
      vfs: new NodeFileSystem({ root: projectRoot }),
      dispose: () => kernel.dispose(),
    };

    const chunks: Uint8Array[] = [];
    const stderrChunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execFileSync } = require('child_process');
      const result = execFileSync('/node_modules/.bin/demo', ['alpha', 'beta'], {
        encoding: 'utf-8',
      });
      process.stdout.write(result);
    `], {
      onStdout: (data) => chunks.push(data),
      onStderr: (data) => stderrChunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    const stderr = stderrChunks.map(c => new TextDecoder().decode(c)).join('');
    expect(stderr).toBe('');
    expect(output.trim()).toBe(JSON.stringify(['alpha', 'beta']));
  });

  it('execFileSync unwraps shell shims whose node entrypoint has no shebang or extension', async () => {
    const projectRoot = mkdtempSync(join(tmpdir(), 'agent-os-node-bin-shim-no-shebang-'));
    cleanupPaths.push(projectRoot);

    mkdirSync(join(projectRoot, 'node_modules', '.bin'), { recursive: true });
    mkdirSync(join(projectRoot, 'node_modules', 'demo', 'dist', 'bin'), { recursive: true });
    writeFileSync(
      join(projectRoot, 'node_modules', 'demo', 'dist', 'bin', 'demo'),
      '"use strict";\nconsole.log(JSON.stringify(process.argv.slice(2)));\n',
    );
    chmodSync(join(projectRoot, 'node_modules', 'demo', 'dist', 'bin', 'demo'), 0o755);
    writeFileSync(
      join(projectRoot, 'node_modules', '.bin', 'demo-no-shebang'),
      [
        '#!/bin/sh',
        'basedir=$(dirname "$0")',
        'if [ -x "$basedir/node" ]; then',
        '  exec "$basedir/node" "$basedir/../demo/dist/bin/demo" "$@"',
        'else',
        '  exec node "$basedir/../demo/dist/bin/demo" "$@"',
        'fi',
        '',
      ].join('\n'),
    );
    chmodSync(join(projectRoot, 'node_modules', '.bin', 'demo-no-shebang'), 0o755);

    const kernel = createKernel({
      filesystem: new NodeFileSystem({ root: projectRoot }),
    });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
    await kernel.mount(createNodeRuntime());
    ctx = {
      kernel,
      vfs: new NodeFileSystem({ root: projectRoot }),
      dispose: () => kernel.dispose(),
    };

    const chunks: Uint8Array[] = [];
    const stderrChunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execFileSync } = require('child_process');
      const result = execFileSync('/node_modules/.bin/demo-no-shebang', ['gamma', 'delta'], {
        encoding: 'utf-8',
      });
      process.stdout.write(result);
    `], {
      onStdout: (data) => chunks.push(data),
      onStderr: (data) => stderrChunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    const stderr = stderrChunks.map(c => new TextDecoder().decode(c)).join('');
    expect(stderr).toBe('');
    expect(output.trim()).toBe(JSON.stringify(['gamma', 'delta']));
  });
});

describeIf(!skipReason, 'bridge child_process exploit/abuse paths', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
  });

  it('child_process cannot escape to host shell', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    // Use a command that produces different output in sandbox vs host:
    // /etc/hostname exists on the host but not in the kernel VFS
    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      try {
        const result = execSync('cat /etc/hostname', { encoding: 'utf-8' });
        // If we get here, the command read a host-only file
        console.log('ESCAPED:' + result.trim());
      } catch (e) {
        // Expected: /etc/hostname doesn't exist in the sandbox VFS
        console.log('sandbox:contained');
      }
    `], {
      onStdout: (data) => chunks.push(data),
    });

    await proc.wait();
    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    // Positive: command ran in sandbox and couldn't access host filesystem
    expect(output).toContain('sandbox:contained');
    // Negative: no host data leaked
    expect(output).not.toContain('ESCAPED:');
  });

  it('child_process cannot read host filesystem', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      try {
        // /etc/passwd doesn't exist in the kernel VFS
        execSync('cat /etc/passwd', { encoding: 'utf-8' });
        console.log('SECURITY_BREACH');
      } catch (e) {
        console.log('blocked');
      }
    `], {
      onStdout: (data) => chunks.push(data),
    });

    await proc.wait();
    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).not.toContain('SECURITY_BREACH');
    expect(output).toContain('blocked');
  });

  it('child_process write goes to kernel VFS not host', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const chunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', `
      const { execSync } = require('child_process');
      execSync('echo "written-by-child" > /tmp/child-output.txt');
      const result = execSync('cat /tmp/child-output.txt', { encoding: 'utf-8' });
      console.log(result.trim());
    `], {
      onStdout: (data) => chunks.push(data),
    });

    const code = await proc.wait();
    expect(code).toBe(0);

    const output = chunks.map(c => new TextDecoder().decode(c)).join('');
    expect(output).toContain('written-by-child');

    // Verify the file was written to kernel VFS
    const content = await ctx.vfs.readFile('/tmp/child-output.txt');
    expect(new TextDecoder().decode(content)).toContain('written-by-child');
  });
});
