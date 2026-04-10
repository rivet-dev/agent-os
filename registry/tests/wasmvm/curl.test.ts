/**
 * Integration tests for curl and the C socket layer (host_socket.c).
 *
 * Tests the WASM socket implementation that powers curl:
 *   - DNS resolution (getaddrinfo)
 *   - TCP socket creation and connection
 *   - Non-blocking socket mode (fcntl O_NONBLOCK)
 *   - Socket options (getsockopt SO_ERROR, setsockopt TCP_NODELAY)
 *   - Poll for readability/writability
 *   - HTTP send/recv over raw sockets
 *   - Remote endpoint connectivity
 */

import { describe, it, expect, afterEach, beforeAll, afterAll } from 'vitest';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-core/test/runtime';
import {
  allowAll,
  COMMANDS_DIR,
  createInMemoryFileSystem,
  createKernel,
  hasWasmBinaries,
} from '../helpers.js';
import type { Kernel } from '../helpers.js';
import {
  createServer as createHttpServer,
  type IncomingMessage,
  type Server as HttpServer,
  type ServerResponse,
} from 'node:http';
import { createServer as createHttpsServer, type Server as HttpsServer } from 'node:https';
import {
  createConnection,
  createServer as createTcpServer,
  type Server as TcpServer,
} from 'node:net';
import { execSync } from 'node:child_process';
import { existsSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const CURL_PACKAGE_DIR = resolve(__dirname, '../../software/curl/wasm');

const hasHttpGetTest = hasWasmBinaries && existsSync(resolve(COMMANDS_DIR, 'http_get_test'));
const hasPackagedCurl = existsSync(resolve(CURL_PACKAGE_DIR, 'curl'));
const hasCurl = hasPackagedCurl || (hasWasmBinaries && existsSync(resolve(COMMANDS_DIR, 'curl')));
const runExternalNetwork = process.env.AGENTOS_E2E_NETWORK === '1';
const EXTERNAL_HOST = 'example.com';
const EXTERNAL_TCP_PORT = 80;
const EXTERNAL_HTTP_URL = `http://${EXTERNAL_HOST}/`;
const EXTERNAL_HTTPS_URL = `https://${EXTERNAL_HOST}/`;
const EXTERNAL_EXPECTED_BODY = 'Example Domain';
const EXTERNAL_RETRY_ATTEMPTS = 3;
const EXTERNAL_RETRY_DELAY_MS = 1_000;
const EXTERNAL_PROBE_TIMEOUT_MS = 8_000;
let hasOpenssl = false;

try {
  execSync('openssl version', { stdio: 'pipe' });
  hasOpenssl = true;
} catch {
  hasOpenssl = false;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

function formatError(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

async function retryExternal<T>(run: () => Promise<T>, attempts = EXTERNAL_RETRY_ATTEMPTS): Promise<T> {
  let lastError: unknown;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      return await run();
    } catch (error) {
      lastError = error;
      if (attempt < attempts) {
        await sleep(EXTERNAL_RETRY_DELAY_MS);
      }
    }
  }

  throw lastError ?? new Error('external network probe failed');
}

async function probeExternalTcp(): Promise<void> {
  await new Promise<void>((resolveConnect, rejectConnect) => {
    const socket = createConnection({
      host: EXTERNAL_HOST,
      port: EXTERNAL_TCP_PORT,
    });
    let settled = false;

    const finish = (callback: () => void) => {
      if (settled) return;
      settled = true;
      callback();
    };

    socket.setTimeout(EXTERNAL_PROBE_TIMEOUT_MS);
    socket.once('connect', () => {
      finish(() => {
        socket.end();
        resolveConnect();
      });
    });
    socket.once('timeout', () => {
      finish(() => {
        socket.destroy();
        rejectConnect(new Error(`timed out connecting to ${EXTERNAL_HOST}:${EXTERNAL_TCP_PORT}`));
      });
    });
    socket.once('error', (error) => {
      finish(() => {
        socket.destroy();
        rejectConnect(error);
      });
    });
  });
}

async function probeExternalHttps(): Promise<void> {
  const response = await fetch(EXTERNAL_HTTPS_URL, {
    signal: AbortSignal.timeout(EXTERNAL_PROBE_TIMEOUT_MS),
  });
  if (!response.ok) {
    throw new Error(`host probe failed with HTTP ${response.status}`);
  }
  await response.arrayBuffer();
}

const externalNetworkSkipReason = runExternalNetwork
  ? await (async () => {
      try {
        await retryExternal(async () => {
          await probeExternalTcp();
          await probeExternalHttps();
        });
        return false as const;
      } catch (error) {
        return `external network unavailable: ${formatError(error)}`;
      }
    })()
  : 'set AGENTOS_E2E_NETWORK=1 to enable external-network coverage';

function generateSelfSignedCert(): { key: string; cert: string } {
  const keyPath = join(tmpdir(), `curl-test-key-${process.pid}-${Date.now()}.pem`);
  try {
    const key = execSync(
      'openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 2>/dev/null',
      { encoding: 'utf8' },
    );
    writeFileSync(keyPath, key);
    const cert = execSync(
      `openssl req -new -x509 -key "${keyPath}" -days 1 -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" 2>/dev/null`,
      { encoding: 'utf8' },
    );
    return { key, cert };
  } finally {
    try {
      unlinkSync(keyPath);
    } catch {
      // Best effort cleanup for test temp files.
    }
  }
}

describe.skipIf(!hasCurl && !hasHttpGetTest)('curl and socket layer', () => {
  let kernel: Kernel;
  let httpServer: HttpServer;
  let httpsServer: HttpsServer;
  let keepAliveServer: TcpServer;
  let httpPort: number;
  let httpsPort: number;
  let keepAlivePort: number;
  let flakyRequestCount = 0;

  beforeAll(async () => {
    httpServer = createHttpServer((req: IncomingMessage, res: ServerResponse) => {
      const url = req.url ?? '/';

      if (url === '/json') {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true, path: url }));
        return;
      }

      if (url === '/redirect') {
        res.writeHead(302, { Location: `http://127.0.0.1:${httpPort}/final` });
        res.end();
        return;
      }

      if (url === '/final') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('followed redirect');
        return;
      }

      if (url === '/one') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('first-response\n');
        return;
      }

      if (url === '/two') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('second-response\n');
        return;
      }

      if (url === '/echo' && req.method === 'POST') {
        let body = '';
        req.on('data', (chunk) => {
          body += chunk;
        });
        req.on('end', () => {
          res.writeHead(200, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({
            method: req.method,
            body,
            header: req.headers['x-test'] ?? null,
          }));
        });
        return;
      }

      if (url === '/json-post' && req.method === 'POST') {
        let body = '';
        req.on('data', (chunk) => {
          body += chunk;
        });
        req.on('end', () => {
          res.writeHead(200, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({
            method: req.method,
            contentType: req.headers['content-type'] ?? null,
            accept: req.headers.accept ?? null,
            body,
          }));
        });
        return;
      }

      if (url === '/head-test') {
        res.writeHead(200, {
          'Content-Type': 'text/plain',
          'X-Test-Header': 'present',
        });
        if (req.method === 'HEAD') {
          res.end();
        } else {
          res.end('body should not appear in HEAD output');
        }
        return;
      }

      if (url === '/auth-required') {
        const auth = req.headers.authorization;
        if (!auth || !auth.startsWith('Basic ')) {
          res.writeHead(401, { 'Content-Type': 'text/plain' });
          res.end('unauthorized');
          return;
        }

        const decoded = Buffer.from(auth.slice(6), 'base64').toString();
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end(`authenticated: ${decoded}`);
        return;
      }

      if (url === '/upload' && req.method === 'POST') {
        const contentType = req.headers['content-type'] ?? '';
        const chunks: Buffer[] = [];
        req.on('data', (chunk) => {
          chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
        });
        req.on('end', () => {
          const body = Buffer.concat(chunks).toString();
          res.writeHead(200, { 'Content-Type': 'text/plain' });
          res.end(
            `multipart: ${contentType.startsWith('multipart/form-data')}\n` +
            `body-contains-file: ${body.includes('upload.txt')}`,
          );
        });
        return;
      }

      if (url === '/binary') {
        const payload = Buffer.alloc(256);
        for (let i = 0; i < payload.length; i++) payload[i] = i & 0xff;
        res.writeHead(200, {
          'Content-Type': 'application/octet-stream',
          'Content-Length': String(payload.length),
        });
        res.end(payload);
        return;
      }

      if (url === '/named.txt') {
        const body = 'downloaded-by-remote-name\n';
        res.writeHead(200, {
          'Content-Type': 'text/plain',
          'Content-Length': String(Buffer.byteLength(body)),
        });
        res.end(body);
        return;
      }

      if (url === '/flaky') {
        flakyRequestCount += 1;
        if (flakyRequestCount === 1) {
          res.writeHead(503, { 'Content-Type': 'text/plain' });
          res.end('retry please');
          return;
        }

        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('retry succeeded');
        return;
      }

      if (url === '/status') {
        res.writeHead(201, { 'Content-Type': 'text/plain' });
        res.end('created');
        return;
      }

      res.writeHead(404, { 'Content-Type': 'text/plain' });
      res.end('not found');
    });

    await new Promise<void>((resolveListen) => {
      httpServer.listen(0, '127.0.0.1', resolveListen);
    });
    httpPort = (httpServer.address() as import('node:net').AddressInfo).port;

    if (hasOpenssl) {
      const tlsCert = generateSelfSignedCert();
      httpsServer = createHttpsServer({ key: tlsCert.key, cert: tlsCert.cert }, (req, res) => {
        if (req.url === '/json') {
          res.writeHead(200, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ secure: true, path: req.url }));
          return;
        }

        if (req.url === '/keepalive') {
          const body = 'hello from tls keepalive';
          res.writeHead(200, {
            'Content-Type': 'text/plain',
            'Content-Length': String(Buffer.byteLength(body)),
            Connection: 'keep-alive',
            'Keep-Alive': 'timeout=60',
          });
          res.end(body);
          return;
        }

        res.writeHead(404, { 'Content-Type': 'text/plain' });
        res.end('not found');
      });
      httpsServer.keepAliveTimeout = 60000;

      await new Promise<void>((resolveListen) => {
        httpsServer.listen(0, '127.0.0.1', resolveListen);
      });
      httpsPort = (httpsServer.address() as import('node:net').AddressInfo).port;
    }

    keepAliveServer = createTcpServer((socket) => {
      socket.once('data', () => {
        const body = 'hello from keepalive';
        socket.write(
          'HTTP/1.1 200 OK\r\n' +
          'Content-Type: text/plain\r\n' +
          `Content-Length: ${Buffer.byteLength(body)}\r\n` +
          'Connection: keep-alive\r\n' +
          'Keep-Alive: timeout=60\r\n' +
          '\r\n' +
          body,
        );
        // Intentionally keep the socket open to exercise curl shutdown logic.
      });
    });

    await new Promise<void>((resolveListen) => {
      keepAliveServer.listen(0, '127.0.0.1', resolveListen);
    });
    keepAlivePort = (keepAliveServer.address() as import('node:net').AddressInfo).port;
  });

  afterAll(async () => {
    if (httpServer) {
      await new Promise<void>((resolveClose) => httpServer.close(() => resolveClose()));
    }
    if (httpsServer) {
      await new Promise<void>((resolveClose) => httpsServer.close(() => resolveClose()));
    }
    if (keepAliveServer) {
      await new Promise<void>((resolveClose) => keepAliveServer.close(() => resolveClose()));
    }
  });

  async function createKernelWithNet() {
    flakyRequestCount = 0;
    const filesystem = createInMemoryFileSystem();
    await (filesystem as any).chmod('/', 0o1777);
    await filesystem.mkdir('/tmp', { recursive: true });
    await (filesystem as any).chmod('/tmp', 0o1777);

    kernel = createKernel({
      filesystem,
      permissions: allowAll,
      loopbackExemptPorts: [httpPort, httpsPort, keepAlivePort],
    });
    const commandDirs = hasPackagedCurl ? [CURL_PACKAGE_DIR, COMMANDS_DIR] : [COMMANDS_DIR];
    await kernel.mount(createWasmVmRuntime({ commandDirs }));
    return kernel;
  }

  async function execWithRetry(command: string) {
    let lastResult: Awaited<ReturnType<typeof kernel.exec>> | undefined;
    for (let attempt = 1; attempt <= EXTERNAL_RETRY_ATTEMPTS; attempt += 1) {
      lastResult = await kernel.exec(command);
      if (lastResult.exitCode === 0) return lastResult;
      if (attempt < EXTERNAL_RETRY_ATTEMPTS) {
        await sleep(EXTERNAL_RETRY_DELAY_MS);
      }
    }

    return lastResult!;
  }

  afterEach(async () => {
    await kernel?.dispose();
  });

  it.skipIf(!hasHttpGetTest)('http_get_test reaches a local HTTP server', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`http_get_test 127.0.0.1 ${httpPort} /json`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('HTTP/1.1 200');
    expect(result.stdout).toContain('"ok":true');
  }, 15000);

  it.skipIf(!hasHttpGetTest)('http_get_test preserves non-blocking connect diagnostics', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`http_get_test 127.0.0.1 ${httpPort} /json`);
    expect(result.exitCode).toBe(0);
    expect(result.stderr).toContain('fcntl F_SETFL(NONBLOCK)=0');
    expect(result.stderr).toMatch(/connect=(0|-1 errno=\d+)/);
    expect(result.stderr).toContain('getsockopt(SO_ERROR)=0 value=0');
    expect(result.stderr).toContain('poll(POLLOUT)=1');
  }, 15000);

  it.skipIf(!hasCurl)('curl GET returns JSON from a local server', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -s http://127.0.0.1:${httpPort}/json`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('"ok":true');
  }, 15000);

  it.skipIf(!hasCurl)('curl --version reports the upstream tool version', async () => {
    await createKernelWithNet();
    const result = await kernel.exec('curl --version');
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('curl 8.11.1');
    expect(result.stdout).toMatch(/Protocols:/);
  }, 15000);

  it.skipIf(!hasCurl)('curl -L follows redirects', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -s -L http://127.0.0.1:${httpPort}/redirect`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('followed redirect');
  }, 15000);

  it.skipIf(!hasCurl)('curl POST sends body and headers', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(
      `curl -s -X POST -H 'X-Test: edge-case' -d 'payload-data' http://127.0.0.1:${httpPort}/echo`,
    );
    expect(result.exitCode).toBe(0);
    const body = JSON.parse(result.stdout);
    expect(body.method).toBe('POST');
    expect(body.body).toBe('payload-data');
    expect(body.header).toBe('edge-case');
  }, 15000);

  it.skipIf(!hasCurl)('curl --json sends JSON with the expected headers', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(
      `curl -s --json '{\"hello\":\"world\"}' http://127.0.0.1:${httpPort}/json-post`,
    );
    expect(result.exitCode).toBe(0);
    const body = JSON.parse(result.stdout);
    expect(body.method).toBe('POST');
    expect(body.body).toBe('{"hello":"world"}');
    expect(body.contentType).toBe('application/json');
    expect(body.accept).toBe('application/json');
  }, 15000);

  it.skipIf(!hasCurl)('curl -I returns response headers without the body', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -s -I http://127.0.0.1:${httpPort}/head-test`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('HTTP/');
    expect(result.stdout).toMatch(/X-Test-Header/i);
    expect(result.stdout).not.toContain('body should not appear');
  }, 15000);

  it.skipIf(!hasCurl)('curl -u sends HTTP Basic authentication', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -s -u user:pass http://127.0.0.1:${httpPort}/auth-required`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('authenticated: user:pass');
  }, 15000);

  it.skipIf(!hasCurl)('curl -F uploads multipart form data', async () => {
    await createKernelWithNet();
    await kernel.writeFile('/tmp/upload.txt', 'file payload\n');
    const result = await kernel.exec(`curl -s -F file=@/tmp/upload.txt http://127.0.0.1:${httpPort}/upload`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('multipart: true');
    expect(result.stdout).toContain('body-contains-file: true');
  }, 15000);

  it.skipIf(!hasCurl)('curl -K reads options from a config file', async () => {
    await createKernelWithNet();
    await kernel.writeFile(
      '/tmp/curlrc',
      `silent\nurl = "http://127.0.0.1:${httpPort}/json"\n`,
    );
    const result = await kernel.exec('curl -K /tmp/curlrc');
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('"ok":true');
  }, 15000);

  it.skipIf(!hasCurl)('curl -o writes text output to a file', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -s -o /tmp/out.json http://127.0.0.1:${httpPort}/json`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('');
    const file = new TextDecoder().decode(await kernel.readFile('/tmp/out.json'));
    expect(file).toContain('"ok":true');
  }, 15000);

  it.skipIf(!hasCurl)('curl -o respects the current working directory for relative output paths', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(
      `mkdir -p /tmp/curl-cwd && cd /tmp/curl-cwd && ` +
      `curl -s -o local.txt http://127.0.0.1:${httpPort}/named.txt && cat /tmp/curl-cwd/local.txt`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('downloaded-by-remote-name\n');
  }, 15000);

  it.skipIf(!hasCurl)('curl -o writes binary output without truncation', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -s -o /tmp/out.bin http://127.0.0.1:${httpPort}/binary`);
    expect(result.exitCode).toBe(0);
    const file = await kernel.readFile('/tmp/out.bin');
    expect(file).toHaveLength(256);
    expect(Array.from(file.slice(0, 8))).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);
    expect(Array.from(file.slice(-4))).toEqual([252, 253, 254, 255]);
  }, 15000);

  it.skipIf(!hasCurl)('curl -D and -o split headers and body into separate files', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(
      `curl -s -D /tmp/headers.txt -o /tmp/body.txt http://127.0.0.1:${httpPort}/named.txt`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('');

    const headers = new TextDecoder().decode(await kernel.readFile('/tmp/headers.txt'));
    const body = new TextDecoder().decode(await kernel.readFile('/tmp/body.txt'));
    expect(headers).toContain('HTTP/1.1 200 OK');
    expect(headers).toMatch(/Content-Type: text\/plain/i);
    expect(body).toBe('downloaded-by-remote-name\n');
  }, 15000);

  it.skipIf(!hasCurl)('curl -O writes to the remote filename', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(
      `mkdir -p /tmp/remote-name && cd /tmp/remote-name && ` +
      `curl -s -O http://127.0.0.1:${httpPort}/named.txt && cat /tmp/remote-name/named.txt`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('downloaded-by-remote-name\n');
  }, 15000);

  it.skipIf(!hasCurl)('curl -w writes the HTTP status code', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -s -w '%{http_code}' http://127.0.0.1:${httpPort}/status`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('created');
    expect(result.stdout).toContain('201');
  }, 15000);

  it.skipIf(!hasCurl)('curl -f reports HTTP errors with a non-zero exit code', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -fsS http://127.0.0.1:${httpPort}/missing`);
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toMatch(/404|not found|error/i);
  }, 15000);

  it.skipIf(!hasCurl)('curl --fail-with-body preserves the response body on HTTP errors', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -sS --fail-with-body http://127.0.0.1:${httpPort}/missing`);
    expect(result.exitCode).not.toBe(0);
    expect(result.stdout).toBe('not found');
    expect(result.stderr).toMatch(/404|error/i);
  }, 15000);

  it.skipIf(!hasCurl)('curl reports refused connections without hanging', async () => {
    await createKernelWithNet();

    const probe = createTcpServer();
    await new Promise<void>((resolveListen) => probe.listen(0, '127.0.0.1', resolveListen));
    const unusedPort = (probe.address() as import('node:net').AddressInfo).port;
    await new Promise<void>((resolveClose) => probe.close(() => resolveClose()));

    const startedAt = Date.now();
    const result = await kernel.exec(`curl -sS http://127.0.0.1:${unusedPort}/`);
    expect(result.exitCode).not.toBe(0);
    expect(Date.now() - startedAt).toBeLessThan(8000);
    expect(result.stderr).toMatch(/connect|refused|failed/i);
  }, 15000);

  it.skipIf(!hasCurl)('curl reports DNS failures cleanly', async () => {
    await createKernelWithNet();
    const result = await kernel.exec('curl -sS http://does-not-exist.invalid/');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toMatch(/resolve|host|dns/i);
  }, 15000);

  it.skipIf(!hasCurl)('curl handles multiple URLs in one invocation', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(
      `curl -s http://127.0.0.1:${httpPort}/one http://127.0.0.1:${httpPort}/two`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('first-response\nsecond-response\n');
  }, 15000);

  it.skipIf(!hasCurl)('curl --retry retries transient HTTP failures', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(
      `curl -fsS --retry 2 --retry-delay 0 http://127.0.0.1:${httpPort}/flaky`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('retry succeeded');
    expect(flakyRequestCount).toBeGreaterThanOrEqual(2);
  }, 15000);

  it.skipIf(!hasCurl)('curl exits promptly after a keep-alive response', async () => {
    await createKernelWithNet();
    const startedAt = Date.now();
    const result = await kernel.exec(`curl -s http://127.0.0.1:${keepAlivePort}/`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('hello from keepalive');
    expect(Date.now() - startedAt).toBeLessThan(8000);
  }, 15000);

  it.skipIf(!hasCurl || !hasOpenssl)('curl -k performs an HTTPS request through the WASI TLS backend', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -ks https://127.0.0.1:${httpsPort}/json`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain('"secure":true');
  }, 15000);

  it.skipIf(!hasCurl || !hasOpenssl)('curl fails TLS verification without -k on a self-signed endpoint', async () => {
    await createKernelWithNet();
    const result = await kernel.exec(`curl -sS https://127.0.0.1:${httpsPort}/json`);
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toMatch(/certificate|tls|ssl|verify/i);
  }, 15000);

  it.skipIf(!hasCurl || !hasOpenssl)('curl -k exits promptly after an HTTPS keep-alive response', async () => {
    await createKernelWithNet();
    const startedAt = Date.now();
    const result = await kernel.exec(`curl -ks https://127.0.0.1:${httpsPort}/keepalive`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toBe('hello from tls keepalive');
    expect(Date.now() - startedAt).toBeLessThan(8000);
  }, 15000);

  it.skipIf(!hasHttpGetTest || externalNetworkSkipReason)('http_get_test reaches an external host over real TCP', async () => {
    await createKernelWithNet();
    const result = await execWithRetry(`http_get_test ${EXTERNAL_HOST} ${EXTERNAL_TCP_PORT} /`);
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toMatch(/HTTP\/1\.[01] (200|301|302)/);
  }, 30000);

  it.skipIf(!hasCurl || externalNetworkSkipReason)('curl reaches a real external HTTP endpoint', async () => {
    await createKernelWithNet();
    const result = await execWithRetry(
      `curl -fsSL --retry 2 --retry-delay 1 --retry-all-errors --connect-timeout 10 --max-time 30 ${EXTERNAL_HTTP_URL}`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain(EXTERNAL_EXPECTED_BODY);
  }, 30000);

  it.skipIf(!hasCurl || externalNetworkSkipReason)('curl reaches a real external HTTPS endpoint', async () => {
    await createKernelWithNet();
    const result = await execWithRetry(
      `curl -fsSL --retry 2 --retry-delay 1 --retry-all-errors --connect-timeout 10 --max-time 30 ${EXTERNAL_HTTPS_URL}`,
    );
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain(EXTERNAL_EXPECTED_BODY);
  }, 30000);
});
