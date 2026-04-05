/**
 * Ctrl+C at shell prompt behavior tests.
 *
 * Verifies that pressing Ctrl+C (SIGINT) at the interactive shell prompt:
 * - Echoes ^C and shows a fresh prompt
 * - Does NOT kill the shell process
 * - Discards any partial input on the current line
 * - Allows typing new commands afterward
 *
 * Uses real WasmVM brush-shell, gated by skipUnlessWasmBuilt().
 */

import { describe, it, expect, afterEach } from 'vitest';
import {
  createIntegrationKernel,
  skipUnlessWasmBuilt,
  TerminalHarness,
} from './helpers.ts';
import type { IntegrationKernelResult } from './helpers.ts';

const PROMPT = 'sh-0.4$ ';
const wasmSkip = skipUnlessWasmBuilt();

describe.skipIf(wasmSkip)('Ctrl+C at shell prompt', () => {
  let harness: TerminalHarness;
  let ctx: IntegrationKernelResult;

  afterEach(async () => {
    await harness?.dispose();
    await ctx?.dispose();
  });

  it('partial input + ^C shows ^C, discards input, fresh prompt', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    harness.shell.write('partia\x03');
    await harness.waitFor(PROMPT, 2, 2_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('^C');

    // Fresh prompt on new line
    const lines = screen.split('\n');
    expect(lines[lines.length - 1]).toBe(PROMPT);
  }, 10_000);

  it('empty prompt + ^C shows ^C, fresh prompt, no error', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    harness.shell.write('\x03');
    await harness.waitFor(PROMPT, 2, 2_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('^C');
  }, 10_000);

  it('after ^C at prompt, shell accepts and executes the next command', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);
    // Send ^C, then a real command
    harness.shell.write('partial\x03');
    await harness.waitFor(PROMPT, 2, 2_000);

    await harness.type('echo hello\n');
    await harness.waitFor('hello', 1, 5_000);

    const screen = harness.screenshotTrimmed();
    expect(screen).toContain('hello');
  }, 15_000);

  it('multiple ^C in a row does not crash the shell', async () => {
    ctx = await createIntegrationKernel({ runtimes: ['wasmvm'] });
    harness = new TerminalHarness(ctx.kernel);

    await harness.waitFor(PROMPT);

    // Rapid-fire ^C
    harness.shell.write('\x03');
    await harness.waitFor(PROMPT, 2, 2_000);
    harness.shell.write('\x03');
    await harness.waitFor(PROMPT, 3, 2_000);

    // Shell still alive
    await harness.type('echo still-alive\n');
    await harness.waitFor('still-alive', 1, 5_000);
  }, 15_000);
});
