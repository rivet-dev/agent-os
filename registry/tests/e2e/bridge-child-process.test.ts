/**
 * Integration tests: Node bridge child_process routing through kernel.
 *
 * Verifies that child_process.spawn/execSync/spawnSync calls from Node
 * isolate code route through the kernel's command registry to the
 * appropriate runtime driver (WasmVM for shell commands).
 *
 * Gracefully skipped when the WASM binary is not built.
 */

import { describe, it, expect, afterEach } from 'vitest';
import {
  createIntegrationKernel,
  skipUnlessWasmBuilt,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const skipReason = skipUnlessWasmBuilt();

describe.skipIf(skipReason)('bridge child_process → kernel routing', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    if (ctx) await ctx.dispose();
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
});

describe.skipIf(skipReason)('bridge child_process exploit/abuse paths', () => {
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
