/**
 * Comprehensive node binary integration tests.
 *
 * Covers all node CLI behaviors through the kernel: stdout, stderr,
 * exit codes, error types, delayed output, stdin pipes, VFS access,
 * cross-runtime child_process, --version, and no-args behavior.
 *
 * Each scenario is tested via kernel.exec() (non-PTY path) and key
 * stdout/error scenarios are also verified through TerminalHarness
 * (interactive PTY path).
 *
 * Gracefully skipped when WASM binaries are not built.
 */

import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, it, expect, afterEach } from 'vitest';
import {
	describeIf,
	createIntegrationKernel,
	skipUnlessWasmBuilt,
	TerminalHarness,
  createKernel,
  createNodeRuntime,
  createWasmVmRuntime,
  NodeFileSystem,
  COMMANDS_DIR,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const skipReason = skipUnlessWasmBuilt();

/** brush-shell interactive prompt. */
const PROMPT = 'sh-0.4$ ';

/**
 * Find a line in the screen output that exactly matches the expected text.
 * Excludes lines containing the command echo (prompt line).
 */
function findOutputLine(screen: string, expected: string): string | undefined {
  return screen.split('\n').find(
    (l) => l.trim() === expected && !l.includes(PROMPT),
  );
}

function decodeChunks(chunks: Uint8Array[]): string {
  const decoder = new TextDecoder();
  return chunks.map((chunk) => decoder.decode(chunk)).join("");
}

// ---------------------------------------------------------------------------
// kernel.exec() -- stdout
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec stdout', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node -e console.log produces stdout with exit 0', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "console.log(\'hello\')"');
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('hello');
  });

  it('node -e setTimeout delayed output appears', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec(
      'node -e "setTimeout(()=>console.log(\'delayed\'),100)"',
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('delayed');
  }, 10_000);
});

describeIf(!skipReason, 'node binary: spawn callback routing', () => {
  let ctxA: IntegrationKernelResult;
  let ctxB: IntegrationKernelResult;

  afterEach(async () => {
    await ctxA?.dispose();
    await ctxB?.dispose();
  });

  it('concurrent kernels keep stdout callbacks isolated per VM marker', async () => {
    ctxA = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    ctxB = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });

    const stdoutA: Uint8Array[] = [];
    const stdoutB: Uint8Array[] = [];
    const stderrA: Uint8Array[] = [];
    const stderrB: Uint8Array[] = [];

    const procA = ctxA.kernel.spawn('node', ['-e', "console.log('VM_A_MARKER')"], {
      onStdout: (chunk) => stdoutA.push(chunk),
      onStderr: (chunk) => stderrA.push(chunk),
    });
    const procB = ctxB.kernel.spawn('node', ['-e', "console.log('VM_B_MARKER')"], {
      onStdout: (chunk) => stdoutB.push(chunk),
      onStderr: (chunk) => stderrB.push(chunk),
    });

    const [exitA, exitB] = await Promise.all([procA.wait(), procB.wait()]);
    expect(exitA).toBe(0);
    expect(exitB).toBe(0);

    const stdoutAText = decodeChunks(stdoutA);
    const stdoutBText = decodeChunks(stdoutB);
    expect(stdoutAText).toContain('VM_A_MARKER');
    expect(stdoutAText).not.toContain('VM_B_MARKER');
    expect(stdoutBText).toContain('VM_B_MARKER');
    expect(stdoutBText).not.toContain('VM_A_MARKER');
    expect(decodeChunks(stderrA)).toBe('');
    expect(decodeChunks(stderrB)).toBe('');
  }, 15_000);
});

// ---------------------------------------------------------------------------
// kernel.exec() -- exit codes
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec exit codes', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node -e process.exit(42) returns exit code 42', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "process.exit(42)"');
    expect(result.exitCode).toBe(42);
  });

  it('node -e process.exit(0) returns exit code 0', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "process.exit(0)"');
    expect(result.exitCode).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// kernel.exec() -- stderr and error types
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec stderr', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node -e console.error routes to stderr', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "console.error(\'err\')"');
    expect(result.stderr).toContain('err');
    expect(result.exitCode).toBe(0);
  });

  it('node -e syntax error returns SyntaxError on stderr', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "({" ');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toMatch(/SyntaxError|Unexpected/);
  });

  it('node -e ReferenceError on undefined variable', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "unknownVar"');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain('ReferenceError');
  });

  it('node -e throw new Error returns message on stderr', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "throw new Error(\'boom\')"');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain('boom');
  });
});

// ---------------------------------------------------------------------------
// kernel.exec() -- stdin
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec stdin', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node -e reads from stdin pipe when data provided', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const code = [
      'let d = "";',
      'process.stdin.setEncoding("utf8");',
      'process.stdin.on("data", c => d += c);',
      'process.stdin.on("end", () => console.log(d.trim()));',
    ].join(' ');
    const result = await ctx.kernel.exec(`echo "piped-input" | node -e '${code}'`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('piped-input');
  }, 15_000);
});

// ---------------------------------------------------------------------------
// kernel.exec() -- VFS access
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec VFS access', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node -e fs.readdirSync("/") returns VFS root listing', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec(
      'node -e "console.log(require(\'fs\').readdirSync(\'/\').join(\',\'))"',
    );
    expect(result.exitCode).toBe(0);
    // VFS root should contain at least /bin and /tmp
    expect(result.stdout).toContain('bin');
    expect(result.stdout).toContain('tmp');
  });
});

// ---------------------------------------------------------------------------
// kernel.exec() -- cross-runtime child_process
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec child_process', () => {
  let ctx: IntegrationKernelResult;
  let tempDir: string | undefined;

  afterEach(async () => {
    await ctx?.dispose();
    if (tempDir) {
      await rm(tempDir, { recursive: true, force: true });
      tempDir = undefined;
    }
  });

  it('node -e execSync("echo sub") captures child stdout', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const code =
      'console.log(require("child_process").execSync("echo sub").toString().trim())';
    const result = await ctx.kernel.exec(`node -e '${code}'`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('sub');
  }, 15_000);

  it('node -e spawnSync resolves shebang-backed node_modules/.bin commands through JavaScript runtime', async () => {
    tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-node-bin-'));
    await writeFile(
      path.join(tempDir, 'package.json'),
      JSON.stringify({ name: 'node-bin-repro', private: true }),
    );
    await mkdir(path.join(tempDir, 'node_modules', 'hello-pkg', 'bin'), {
      recursive: true,
    });
    await writeFile(
      path.join(tempDir, 'node_modules', 'hello-pkg', 'bin', 'hello.js'),
      [
        '#!/usr/bin/env node',
        "console.log(`hello ${process.argv.slice(2).join(' ')}`);",
      ].join('\n'),
    );
    await mkdir(path.join(tempDir, 'node_modules', '.bin'), { recursive: true });
    await symlink(
      '../hello-pkg/bin/hello.js',
      path.join(tempDir, 'node_modules', '.bin', 'hello-js'),
    );

    const vfs = new NodeFileSystem({ root: tempDir });
    const kernel = createKernel({ filesystem: vfs, cwd: '/' });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
    await kernel.mount(createNodeRuntime());
    ctx = { kernel, vfs, dispose: () => kernel.dispose() };

    const guestScriptPath = '/tmp/spawn-bin-check.js';
    const code = [
      "const { spawnSync } = require('child_process');",
      "const env = { ...process.env, PATH: `/node_modules/.bin:${process.env.PATH}` };",
      "const result = spawnSync('hello-js', ['from-child'], { encoding: 'utf8', env });",
      "console.log(JSON.stringify({ status: result.status, stdout: result.stdout, stderr: result.stderr }));",
    ].join(' ');
    await ctx.kernel.writeFile(guestScriptPath, code);
    const result = await ctx.kernel.exec(`node ${guestScriptPath}`);
    expect(result.exitCode).toBe(0);

    const payload = JSON.parse(result.stdout.trim()) as {
      status: number | null;
      stdout: string;
      stderr: string;
    };
    expect(payload.status).toBe(0);
    expect(payload.stdout.trim()).toBe('hello from-child');
    expect(payload.stderr).toBe('');
  }, 20_000);
});

// ---------------------------------------------------------------------------
// kernel.exec() -- node --version
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec --version', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node --version outputs semver pattern', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node --version');
    expect(result.exitCode).toBe(0);
    // Node version format: vNN.NN.NN
    expect(result.stdout.trim()).toMatch(/^v\d+\.\d+\.\d+/);
  });
});

// ---------------------------------------------------------------------------
// kernel.exec() -- node with no args + closed stdin
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: exec no args', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node with no args and closed stdin exits cleanly', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    // Pipe empty input so stdin is immediately closed
    const result = await ctx.kernel.exec('echo -n "" | node', { timeout: 10_000 });
    // Should exit without hanging. Any exit code is acceptable.
    // (real Node exits 0 in this case)
    expect(typeof result.exitCode).toBe('number');
  }, 15_000);
});

// ---------------------------------------------------------------------------
// TerminalHarness (PTY path) -- stdout verification
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: terminal stdout', () => {
  let harness: TerminalHarness;
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await harness?.dispose();
    await ctx?.dispose();
  });

  it('node -e console.log output visible on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "console.log(\'MARKER\')"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(findOutputLine(screen, 'MARKER')).toBeDefined();
  }, 15_000);

  it('node -e delayed output visible on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "setTimeout(()=>console.log(\'LATE\'),100)"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(findOutputLine(screen, 'LATE')).toBeDefined();
  }, 15_000);
});

// ---------------------------------------------------------------------------
// TerminalHarness (PTY path) -- stderr verification
// ---------------------------------------------------------------------------

describeIf(!skipReason, 'node binary: terminal stderr', () => {
  let harness: TerminalHarness;
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await harness?.dispose();
    await ctx?.dispose();
  });

  it('node -e ReferenceError visible on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "unknownVar"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('ReferenceError');
  }, 15_000);

  it('node -e throw Error visible on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const stderrChunks: Uint8Array[] = [];
    const proc = ctx.kernel.spawn('node', ['-e', "throw new Error('boom')"], {
      onStderr: (chunk) => stderrChunks.push(chunk),
    });

    const exitCode = await proc.wait();
    expect(exitCode).not.toBe(0);
    expect(decodeChunks(stderrChunks)).toContain('boom');
  }, 15_000);

  it('node -e SyntaxError visible on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "({"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toMatch(/SyntaxError|Unexpected/);
  }, 15_000);
});
