/**
 * Integration tests for the upstream DuckDB CLI build.
 *
 * Verifies that the registry's source-built DuckDB binary works end-to-end with
 * the shared WASI/POSIX runtime:
 *   - basic in-memory SQL execution
 *   - joins, indexes, and temp tables
 *   - persistent database files on the kernel VFS
 *   - crash recovery for uncommitted transactions
 *   - spill-to-disk via temp files
 *   - CSV ingestion from the kernel filesystem
 *   - remote fetch via the shared WASI/POSIX network stack followed by DuckDB query
 */

import { describe, it, expect, afterEach } from 'vitest';
import {
  COMMANDS_DIR,
  C_BUILD_DIR,
  allowAll,
  createInMemoryFileSystem,
  createKernel,
  createNodeHostNetworkAdapter,
  createWasmVmRuntime,
} from '../helpers.js';
import type { Kernel } from '../helpers.js';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

const hasWasmDuckDB = existsSync(resolve(C_BUILD_DIR, 'duckdb'));
const hasWasmCurl =
  (existsSync(resolve(COMMANDS_DIR, 'curl')) || existsSync(resolve(C_BUILD_DIR, 'curl')));
const hasWasmHttpGet = existsSync(resolve(C_BUILD_DIR, 'http_get'));

async function mountKernel(
  filesystem: ReturnType<typeof createInMemoryFileSystem>,
) {
  const kernel = createKernel({
    filesystem,
    cwd: '/tmp',
    permissions: allowAll,
    hostNetworkAdapter: createNodeHostNetworkAdapter(),
  });
  const commandDirs = existsSync(COMMANDS_DIR) ? [C_BUILD_DIR, COMMANDS_DIR] : [C_BUILD_DIR];
  await kernel.mount(
    createWasmVmRuntime({
      commandDirs,
      permissions: {
        duckdb: 'read-write',
        http_get: 'full',
      },
    })
  );
  return kernel;
}

function closeServer(server: Server) {
  return new Promise<void>((resolve, reject) => {
    server.close((err) => {
      if (err) reject(err);
      else resolve();
    });
  });
}

async function waitForText(
  getText: () => string,
  expected: string,
  timeoutMs = 5_000,
) {
  const start = Date.now();
  while (!getText().includes(expected)) {
    if (Date.now() - start >= timeoutMs) {
      throw new Error(`timed out waiting for output: ${expected}\n\n${getText()}`);
    }
    await sleep(25);
  }
}

describe.skipIf(!hasWasmDuckDB)('duckdb command', { timeout: 120_000 }, () => {
  let kernel: Kernel | undefined;

  afterEach(async () => {
    await kernel?.dispose();
    kernel = undefined;
  });

  it('executes basic SQL against an in-memory database', async () => {
    const filesystem = createInMemoryFileSystem();
    await filesystem.mkdir('/tmp');
    kernel = await mountKernel(filesystem);

    const result = await kernel.exec('duckdb -csv -c "SELECT 41 + 1 AS answer"');
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('answer\n42');
  });

  it('persists database files on the shared VFS and reopens them in a new process', async () => {
    const filesystem = createInMemoryFileSystem();
    await filesystem.mkdir('/tmp');
    await filesystem.writeFile('/tmp/input.csv', 'name,value\nalpha,1\nbeta,2\n');

    kernel = await mountKernel(filesystem);
    let result = await kernel.exec(
      `duckdb -csv /tmp/app.duckdb -c "CREATE TABLE items AS SELECT * FROM read_csv_auto('/tmp/input.csv');"`
    );
    expect(result.exitCode).toBe(0);
    await kernel.dispose();
    kernel = undefined;

    expect(await filesystem.exists('/tmp/app.duckdb')).toBe(true);
    expect((await filesystem.stat('/tmp/app.duckdb')).size).toBeGreaterThan(0);

    kernel = await mountKernel(filesystem);
    result = await kernel.exec(
      `duckdb -csv /tmp/app.duckdb -c "SELECT name, value FROM items ORDER BY value;"`
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('name,value\nalpha,1\nbeta,2');
  });

  it('persists inserted and updated rows across process reopens', async () => {
    const filesystem = createInMemoryFileSystem();
    await filesystem.mkdir('/tmp');
    kernel = await mountKernel(filesystem);

    let result = await kernel.exec(
      `duckdb -csv /tmp/dml.duckdb -c "CREATE TABLE items(id INTEGER, value INTEGER); INSERT INTO items VALUES (1, 10), (2, 20); UPDATE items SET value = value + 1 WHERE id = 2;"`
    );
    expect(result.exitCode).toBe(0);

    result = await kernel.exec(
      `duckdb -csv /tmp/dml.duckdb -c "SELECT id, value FROM items ORDER BY id;"`
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('id,value\n1,10\n2,21');
  });

  it('supports joins and indexes on file-backed tables', async () => {
    const filesystem = createInMemoryFileSystem();
    await filesystem.mkdir('/tmp');
    kernel = await mountKernel(filesystem);

    const result = await kernel.exec(
      `duckdb -csv /tmp/analytics.duckdb -c "CREATE TABLE numbers AS SELECT i AS id, i * 10 AS score FROM range(1, 6) tbl(i); CREATE TABLE labels AS SELECT i AS id, concat('n', CAST(i AS VARCHAR)) AS name FROM range(1, 6) tbl(i); CREATE INDEX idx_numbers_id ON numbers(id); SELECT name, score FROM numbers JOIN labels USING (id) WHERE id >= 3 ORDER BY id;"`
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('name,score\nn3,30\nn4,40\nn5,50');
  });

  it('keeps temp tables scoped to a single DuckDB process', async () => {
    const filesystem = createInMemoryFileSystem();
    await filesystem.mkdir('/tmp');
    kernel = await mountKernel(filesystem);

    let result = await kernel.exec(
      `duckdb -csv /tmp/session.duckdb -c "CREATE TEMP TABLE session_values AS SELECT 7 AS value; SELECT value FROM session_values;"`
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('value\n7');

    result = await kernel.exec(
      `duckdb -csv /tmp/session.duckdb -c "SELECT value FROM session_values;"`
    );
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain('session_values');
  });

  it('drops uncommitted rows after a hard-killed process is reopened', async () => {
    const filesystem = createInMemoryFileSystem();
    await filesystem.mkdir('/tmp');
    kernel = await mountKernel(filesystem);

    let result = await kernel.exec(
      `duckdb -csv /tmp/recover.duckdb -c "CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (1);"`
    );
    expect(result.exitCode).toBe(0);

    let stdout = '';
    const proc = kernel.spawn('duckdb', ['-csv', '/tmp/recover.duckdb'], {
      streamStdin: true,
      onStdout: (chunk) => {
        stdout += new TextDecoder().decode(chunk);
      },
    });

    await sleep(300);
    proc.writeStdin('BEGIN;\nINSERT INTO items VALUES (42);\nSELECT COUNT(*) AS rows_in_tx FROM items;\n');
    await waitForText(() => stdout, 'rows_in_tx\n2');

    proc.kill(9);
    await proc.wait().catch(() => undefined);

    result = await kernel.exec(
      `duckdb -csv /tmp/recover.duckdb -c "SELECT COUNT(*) AS rows, SUM(value) AS total FROM items;"`
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe('rows,total\n1,1');
  });

  it('handles large sorted exports with a configured temp directory under constrained memory', async () => {
    const filesystem = createInMemoryFileSystem();
    await filesystem.mkdir('/tmp');
    kernel = await mountKernel(filesystem);

    const result = await kernel.exec(
      `duckdb -csv /tmp/spill.duckdb -c "PRAGMA temp_directory='/tmp/duckdb-spill'; SET threads=1; SET preserve_insertion_order=false; SET memory_limit='64MB'; COPY (SELECT i, repeat('x', 256) AS payload FROM range(300000) tbl(i) ORDER BY i DESC) TO '/tmp/spilled.csv' (HEADER, DELIMITER ',');"`
    );
    expect(result.exitCode).toBe(0);
    expect(await filesystem.exists('/tmp/spilled.csv')).toBe(true);
    expect((await filesystem.stat('/tmp/spilled.csv')).size).toBeGreaterThan(50_000_000);
  });

  it.skipIf(!hasWasmCurl && !hasWasmHttpGet)(
    'queries data fetched over the network through the shared VFS',
    async () => {
      const filesystem = createInMemoryFileSystem();
      await filesystem.mkdir('/tmp');
      kernel = await mountKernel(filesystem);

      const server = createServer((req: IncomingMessage, res: ServerResponse) => {
        if (req.url === '/' || req.url === '/remote.csv') {
          res.writeHead(200, { 'Content-Type': 'text/csv' });
          res.end('city,value\nsf,3\nla,5\n');
          return;
        }

        res.writeHead(404, { 'Content-Type': 'text/plain' });
        res.end('not found');
      });

      await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));

      try {
        const address = server.address();
        if (!address || typeof address === 'string') {
          throw new Error('failed to bind test HTTP server');
        }

        let result;
        if (hasWasmHttpGet) {
          result = await kernel.exec(
            `http_get ${address.port} /remote.csv /tmp/remote.csv`
          );
          expect(result.exitCode).toBe(0);
        } else {
          result = await kernel.exec(
            `curl -fsS -o /tmp/remote.csv http://127.0.0.1:${address.port}/remote.csv`
          );
          expect(result.exitCode).toBe(0);
        }

        expect(await filesystem.readTextFile('/tmp/remote.csv')).toContain('city,value');

        result = await kernel.exec(
          `duckdb -csv -c "SELECT SUM(value) AS total FROM read_csv_auto('/tmp/remote.csv');"`
        );
        expect(result.exitCode).toBe(0);
        expect(result.stdout.trim()).toBe('total\n8');
      } finally {
        await closeServer(server);
      }
    }
  );
});
