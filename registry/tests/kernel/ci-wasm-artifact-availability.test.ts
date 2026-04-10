/**
 * CI guard for cross-runtime network Wasm artifacts.
 *
 * The cross-runtime network suite now uses first-party command artifacts only.
 * It may skip locally when those binaries are absent, but CI must fail before
 * that suite can silently disappear behind skip guards.
 */

import { describe, it, expect } from 'vitest';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { COMMANDS_DIR } from './helpers.ts';

const REQUIRED_ARTIFACTS = [
  {
    label: 'Wasm command directory',
    path: COMMANDS_DIR,
    buildStep: 'run `make wasm` in `native/`',
  },
  {
    label: 'curl WASM binary',
    path: join(COMMANDS_DIR, 'curl'),
    buildStep: 'run `make wasm` in `native/`',
  },
] as const;

function formatMissingArtifacts(): string {
  return REQUIRED_ARTIFACTS
    .filter((artifact) => !existsSync(artifact.path))
    .map((artifact) => `- ${artifact.label}: missing at ${artifact.path} (${artifact.buildStep})`)
    .join('\n');
}

describe('Kernel cross-runtime CI Wasm artifact availability', () => {
  it.skipIf(!process.env.CI)('requires cross-runtime Wasm fixtures in CI', () => {
    const missing = formatMissingArtifacts();
    expect(
      missing,
      missing === ''
        ? undefined
        : `Missing required Wasm artifacts in CI:\n${missing}`,
    ).toBe('');
  });
});
