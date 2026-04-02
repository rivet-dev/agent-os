/**
 * Cross-runtime terminal tests for the post-Python WasmVM + Node surface.
 *
 * Mounts WasmVM + Node into the same kernel and verifies interactive output
 * through TerminalHarness.
 *
 * Gated: WasmVM binaries must be built.
 *
 * Uses the repo-owned TerminalHarness exported through the Agent OS core
 * test runtime surface.
 */

import { describe, it, expect, afterEach } from 'vitest';
import {
  createIntegrationKernel,
  skipUnlessWasmBuilt,
  TerminalHarness,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

/** brush-shell interactive prompt. */
const PROMPT = 'sh-0.4$ ';

const wasmSkip = skipUnlessWasmBuilt();

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
// Node cross-runtime terminal tests
// ---------------------------------------------------------------------------

describe.skipIf(wasmSkip)('cross-runtime terminal: node', () => {
  let harness: TerminalHarness;
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await harness?.dispose();
    await ctx?.dispose();
  });

  it('node -e stdout appears as actual output (not just command echo)', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    // Use XYZZY. Unique string that does NOT appear in the command text.
    await harness.type('node -e "console.log(\'XYZZY\')"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    // Verify output on its own line (not just embedded in command echo)
    expect(findOutputLine(screen, 'XYZZY')).toBeDefined();
    // Verify prompt returned
    const lines = screen.split('\n');
    expect(lines[lines.length - 1]).toBe(PROMPT);
  }, 15_000);

  it('node -e multiple console.log lines appear in order', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "console.log(\'AAA\'); console.log(\'BBB\')"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(findOutputLine(screen, 'AAA')).toBeDefined();
    expect(findOutputLine(screen, 'BBB')).toBeDefined();

    // Verify order: AAA before BBB
    const aaaIdx = screen.indexOf('AAA');
    const bbbIdx = screen.indexOf('BBB');
    // Both must appear after command echo
    const promptIdx = screen.indexOf(PROMPT);
    expect(aaaIdx).toBeGreaterThan(promptIdx);
    expect(bbbIdx).toBeGreaterThan(aaaIdx);
  }, 15_000);

  it('WARN message does not suppress real stdout', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "console.log(\'HELLO\')"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    // Both the WARN and actual output must coexist
    expect(screen).toContain('WARN');
    expect(findOutputLine(screen, 'HELLO')).toBeDefined();
  }, 15_000);

  it('^C during node -e. Shell survives and prompt returns', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    // Start a long-running node process
    harness.shell.write('node -e "setTimeout(() => {}, 60000)"\n');

    // Give it a moment to start, then send ^C
    await new Promise((r) => setTimeout(r, 500));
    harness.shell.write('\x03');

    // Wait for prompt to return
    await harness.waitFor(PROMPT, 2, 10_000);

    // Verify shell is still alive. Type another command.
    await harness.type('echo alive\n');
    await harness.waitFor('alive', 1, 5_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('alive');
  }, 20_000);
});

// ---------------------------------------------------------------------------
// Node kernel.exec() stdout tests
// ---------------------------------------------------------------------------

describe.skipIf(wasmSkip)('cross-runtime exec: node', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('kernel.exec node -e stdout contains output', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "console.log(42)"');
    expect(result.stdout).toContain('42');
    expect(result.exitCode).toBe(0);
  });

  it('kernel.exec node -e multi-line stdout in order', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec(
      'node -e "console.log(1); console.log(2)"',
    );
    const lines = result.stdout.split('\n').map((l: string) => l.trim()).filter(Boolean);
    expect(lines).toContain('1');
    expect(lines).toContain('2');
    expect(lines.indexOf('1')).toBeLessThan(lines.indexOf('2'));
  });

  it('kernel.exec node -e large stdout does not truncate', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    // Generate >64KB of output (100 lines of 700 chars each = ~70KB)
    const code = `for(let i=0;i<100;i++) console.log('L'+i+' '+'x'.repeat(700))`;
    const result = await ctx.kernel.exec(`node -e "${code}"`);
    // Verify first and last lines present
    expect(result.stdout).toContain('L0 ');
    expect(result.stdout).toContain('L99 ');
    expect(result.exitCode).toBe(0);
  }, 15_000);
});

// ---------------------------------------------------------------------------
// Node kernel.exec() stderr tests
// ---------------------------------------------------------------------------

describe.skipIf(wasmSkip)('cross-runtime exec: node stderr', () => {
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await ctx?.dispose();
  });

  it('kernel.exec node -e with undefined var returns ReferenceError on stderr', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "lskdjf"');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain('ReferenceError');
  });

  it('kernel.exec node -e throw Error returns message on stderr', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "throw new Error(\'boom\')"');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain('boom');
  });

  it('kernel.exec node -e with syntax error returns SyntaxError on stderr', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "({"');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain('SyntaxError');
  });

  it('kernel.exec node -e console.error returns stderr', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    const result = await ctx.kernel.exec('node -e "console.error(\'ERRMSG\')"');
    expect(result.stderr).toContain('ERRMSG');
    expect(result.exitCode).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// Node cross-runtime terminal: stderr tests
// ---------------------------------------------------------------------------

describe.skipIf(wasmSkip)('cross-runtime terminal: node stderr', () => {
  let harness: TerminalHarness;
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await harness?.dispose();
    await ctx?.dispose();
  });

  it('node -e with undefined var shows ReferenceError on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "lskdjf"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('ReferenceError');
  }, 15_000);

  it('node -e throw Error shows error message on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "throw new Error(\'boom\')"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('boom');
  }, 15_000);

  it('node -e syntax error shows SyntaxError on terminal', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    await harness.type('node -e "({"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('SyntaxError');
  }, 15_000);

  it('stderr callback chain: NodeRuntimeDriver -> ctx.onStderr -> PTY slave', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    // console.error goes through onStdio -> ctx.onStderr -> PTY write
    await harness.type('node -e "console.error(\'STDERRTEST\')"\n');
    await harness.waitFor(PROMPT, 2, 10_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('STDERRTEST');
  }, 15_000);
});
