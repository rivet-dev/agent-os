/**
 * CI guard for cross-runtime network Wasm artifacts.
 *
 * The cross-runtime network suite skips locally when these binaries are absent,
 * but CI must fail before that suite can silently disappear behind skip guards.
 */

import { describe, it, expect } from 'vitest';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { COMMANDS_DIR, C_BUILD_DIR } from './helpers.ts';

const REQUIRED_ARTIFACTS = [
  {
    label: 'Wasm command directory',
    path: COMMANDS_DIR,
    buildStep: 'run `make wasm` in `native/`',
  },
  {
    label: 'tcp_server C WASM binary',
    path: join(C_BUILD_DIR, 'tcp_server'),
    buildStep: 'run `make -C native/c sysroot && make -C native/c programs`',
  },
  {
    label: 'http_get C WASM binary',
    path: join(C_BUILD_DIR, 'http_get'),
    buildStep: 'run `make -C native/c sysroot && make -C native/c programs`',
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
