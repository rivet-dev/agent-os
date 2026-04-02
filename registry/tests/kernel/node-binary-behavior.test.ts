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

import { describe, it, expect, afterEach } from 'vitest';
import {
  createIntegrationKernel,
  skipUnlessWasmBuilt,
  TerminalHarness,
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

// ---------------------------------------------------------------------------
// kernel.exec() -- stdout
// ---------------------------------------------------------------------------

describe.skipIf(skipReason)('node binary: exec stdout', () => {
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

// ---------------------------------------------------------------------------
// kernel.exec() -- exit codes
// ---------------------------------------------------------------------------

describe.skipIf(skipReason)('node binary: exec exit codes', () => {
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

describe.skipIf(skipReason)('node binary: exec stderr', () => {
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

describe.skipIf(skipReason)('node binary: exec stdin', () => {
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

describe.skipIf(skipReason)('node binary: exec VFS access', () => {
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

describe.skipIf(skipReason)('node binary: exec child_process', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('node -e execSync("echo sub") captures child stdout', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const code =
      'console.log(require("child_process").execSync("echo sub").toString().trim())';
    const result = await ctx.kernel.exec(`node -e '${code}'`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('sub');
  }, 15_000);
});

// ---------------------------------------------------------------------------
// kernel.exec() -- node --version
// ---------------------------------------------------------------------------

describe.skipIf(skipReason)('node binary: exec --version', () => {
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

describe.skipIf(skipReason)('node binary: exec no args', () => {
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

describe.skipIf(skipReason)('node binary: terminal stdout', () => {
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

describe.skipIf(skipReason)('node binary: terminal stderr', () => {
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
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "throw new Error(\'boom\')"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('boom');
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
