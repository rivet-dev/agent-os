/**
 * Cross-runtime network integration tests.
 *
 * These suites stay on the runtime matrix that currently routes through the
 * shared kernel transport path in this repo: guest Node.js and guest WASM
 * commands. They intentionally use shipped first-party command artifacts so the
 * suite stays runnable without optional `native/c` fixture builds.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { existsSync } from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { resolve } from 'node:path';
import { createServer as createNetServer } from 'node:net';
import {
  COMMANDS_DIR,
  createIntegrationKernel,
  skipUnlessWasmBuilt,
} from './helpers.ts';
import type { IntegrationKernelResult, Kernel } from './helpers.ts';

const WASM_CURL = resolve(COMMANDS_DIR, 'curl');

function skipReasonNetwork(): string | false {
  const wasmSkipReason = skipUnlessWasmBuilt();
  if (wasmSkipReason) return wasmSkipReason;
  if (!existsSync(WASM_CURL)) {
    return `curl WASM binary not found at ${WASM_CURL} — rebuild registry command artifacts`;
  }
  return false;
}

interface RunningGuestProgram {
  process: ReturnType<Kernel['spawn']>;
  stdoutChunks: Uint8Array[];
  stderrChunks: Uint8Array[];
  getExitCode: () => number | null;
}

function decodeChunks(chunks: Uint8Array[]): string {
  return chunks.map((chunk) => new TextDecoder().decode(chunk)).join('');
}

function spawnGuestNodeProgram(
  kernel: Kernel,
  code: string,
): RunningGuestProgram {
  const stdoutChunks: Uint8Array[] = [];
  const stderrChunks: Uint8Array[] = [];
  let exitCode: number | null = null;
  const process = kernel.spawn('node', ['-e', code], {
    onStdout: (chunk) => stdoutChunks.push(chunk),
    onStderr: (chunk) => stderrChunks.push(chunk),
  });
  void process.wait().then((code) => {
    exitCode = code;
  });
  return {
    process,
    stdoutChunks,
    stderrChunks,
    getExitCode: () => exitCode,
  };
}

async function runGuestNodeProgram(
  kernel: Kernel,
  code: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const program = spawnGuestNodeProgram(kernel, code);
  const exitCode = await program.process.wait();
  return {
    exitCode,
    stdout: decodeChunks(program.stdoutChunks),
    stderr: decodeChunks(program.stderrChunks),
  };
}

describe.skipIf(skipReasonNetwork())('cross-runtime network integration', { timeout: 30_000 }, () => {
  let ctx: IntegrationKernelResult;
  let hostNetServer: ReturnType<typeof createNetServer> | undefined;
  let hostHttpServer: ReturnType<typeof createHttpServer> | undefined;

  afterEach(async () => {
    if (hostNetServer) {
      await new Promise<void>((resolveClose) => hostNetServer!.close(() => resolveClose()));
      hostNetServer = undefined;
    }
    if (hostHttpServer) {
      await new Promise<void>((resolveClose) => hostHttpServer!.close(() => resolveClose()));
      hostHttpServer = undefined;
    }
    await ctx?.dispose();
  });

  it('Node.js net.connect resolves localhost and exchanges TCP data over guest loopback', async () => {
    hostNetServer = createNetServer((socket) => {
      socket.on('data', (chunk) => {
        socket.end(`pong:${chunk.toString()}`);
      });
    });
    await new Promise<void>((resolveListen) => {
      hostNetServer!.listen(0, '127.0.0.1', () => resolveListen());
    });
    const port = (hostNetServer.address() as import('node:net').AddressInfo).port;
    ctx = await createIntegrationKernel({
      runtimes: ['node'],
      loopbackExemptPorts: [port],
    });

    const clientResult = await runGuestNodeProgram(
      ctx.kernel,
      [
      "const dns = require('dns');",
      "const net = require('net');",
      'async function main() {',
      "  const lookup = await dns.promises.lookup('localhost', { family: 4 });",
      '  const reply = await new Promise((resolve, reject) => {',
      `    const client = net.connect({ host: 'localhost', port: ${port}, family: 4 }, () => {`,
      "      client.write('ping');",
      '    });',
      "    client.on('data', (chunk) => {",
      '      resolve(chunk.toString());',
      '      client.end();',
      '    });',
      "    client.on('error', reject);",
      '  });',
      '  console.log(JSON.stringify({ lookup, reply }));',
      '}',
      'main().catch((error) => {',
      '  console.error(error);',
      '  process.exit(1);',
      '});',
      ].join('\n'),
    );

    expect(clientResult.exitCode).toBe(0);
    expect(clientResult.stderr).toBe('');
    const parsed = JSON.parse(clientResult.stdout.trim()) as {
      lookup: { address: string };
      reply: string;
    };
    expect(parsed.lookup.address).toBe('127.0.0.1');
    expect(parsed.reply).toBe('pong:ping');
  });

  it('Wasm curl reaches a guest Node.js HTTP server over 127.0.0.1 loopback', async () => {
    hostHttpServer = createHttpServer((req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          host: req.headers.host ?? null,
          url: req.url,
          runtime: 'host',
        }),
      );
    });
    await new Promise<void>((resolveListen) => {
      hostHttpServer!.listen(0, '127.0.0.1', () => resolveListen());
    });
    const port = (hostHttpServer.address() as import('node:net').AddressInfo).port;
    ctx = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
      loopbackExemptPorts: [port],
    });

    const result = await ctx.kernel.exec(`curl -s http://127.0.0.1:${port}/loopback`);

    expect(result.exitCode).toBe(0);
    expect(result.stderr).not.toContain('socket error');
    expect(result.stderr).not.toContain('ERROR');
    expect(result.stdout).toContain('"runtime":"host"');
    expect(result.stdout).toContain('"url":"/loopback"');
    expect(result.stdout).toContain(`"host":"127.0.0.1:${port}"`);
  });

  it('Wasm curl resolves localhost and reaches the loopback fixture through the same kernel path', async () => {
    hostHttpServer = createHttpServer((req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          host: req.headers.host ?? null,
          url: req.url,
          runtime: 'host',
        }),
      );
    });
    await new Promise<void>((resolveListen) => {
      hostHttpServer!.listen(0, '127.0.0.1', () => resolveListen());
    });
    const port = (hostHttpServer.address() as import('node:net').AddressInfo).port;
    ctx = await createIntegrationKernel({
      runtimes: ['wasmvm', 'node'],
      loopbackExemptPorts: [port],
    });

    const result = await ctx.kernel.exec(`curl -s http://localhost:${port}/dns`);

    expect(result.exitCode).toBe(0);
    expect(result.stderr).not.toContain('socket error');
    expect(result.stderr).not.toContain('ERROR');
    expect(result.stdout).toContain('"runtime":"host"');
    expect(result.stdout).toContain('"url":"/dns"');
    expect(result.stdout).toContain(`"host":"localhost:${port}"`);
  });
});
