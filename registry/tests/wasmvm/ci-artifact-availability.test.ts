/**
 * CI guard for story-critical C-built Wasm artifacts.
 *
 * These artifacts back the WasmVM TCP/UDP/Unix/signal integration suites.
 * Local development still skips those suites when the binaries are absent,
 * but CI must fail loudly instead of reporting a green skip-only run.
 */

import { describe, it, expect } from 'vitest';
import { COMMANDS_DIR, C_BUILD_DIR, itIf } from '../helpers.js';
import { existsSync } from 'node:fs';
import { join } from 'node:path';


const REQUIRED_ARTIFACTS = [
  {
    label: 'Wasm command directory',
    path: COMMANDS_DIR,
    buildStep: 'run `make wasm` in `native/wasmvm/`',
  },
  {
    label: 'tcp_server C WASM binary',
    path: join(C_BUILD_DIR, 'tcp_server'),
    buildStep: 'run `make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs`',
  },
  {
    label: 'udp_echo C WASM binary',
    path: join(C_BUILD_DIR, 'udp_echo'),
    buildStep: 'run `make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs`',
  },
  {
    label: 'unix_socket C WASM binary',
    path: join(C_BUILD_DIR, 'unix_socket'),
    buildStep: 'run `make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs`',
  },
  {
    label: 'signal_handler C WASM binary',
    path: join(C_BUILD_DIR, 'signal_handler'),
    buildStep: 'run `make -C native/wasmvm/c sysroot && make -C native/wasmvm/c programs`',
  },
] as const;

function formatMissingArtifacts(): string {
  return REQUIRED_ARTIFACTS
    .filter((artifact) => !existsSync(artifact.path))
    .map((artifact) => `- ${artifact.label}: missing at ${artifact.path} (${artifact.buildStep})`)
    .join('\n');
}

describe('WasmVM CI artifact availability', () => {
  itIf(Boolean(process.env.CI), 'requires story-critical C-built Wasm artifacts in CI', () => {
    const missing = formatMissingArtifacts();
    expect(
      missing,
      missing === ''
        ? undefined
        : `Missing required Wasm artifacts in CI:\n${missing}`,
    ).toBe('');
  });
});
