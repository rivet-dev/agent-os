/**
 * Cross-runtime network integration tests.
 *
 * Verifies that WasmVM and Node.js can communicate via kernel sockets
 * through loopback routing. Neither connection touches the host network.
 *
 * Test 1: WasmVM tcp_server -> Node.js net.connect client
 * Test 2: Node.js http.createServer -> WasmVM http_get client
 *
 * Skipped when WASM binaries are not built.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import {
  AF_INET,
  createInMemoryFileSystem,
  COMMANDS_DIR,
  C_BUILD_DIR,
  createKernel,
  createWasmVmRuntime,
  createNodeRuntime,
  SOCK_STREAM,
} from './helpers.ts';
import type { Kernel } from './helpers.ts';

function skipReasonNetwork(): string | false {
  if (!existsSync(COMMANDS_DIR)) return 'WASM binaries not built (run make wasm in native/)';
  if (!existsSync(join(C_BUILD_DIR, 'tcp_server'))) return 'tcp_server not built (run make -C native/c sysroot && make -C native/c programs)';
  if (!existsSync(join(C_BUILD_DIR, 'http_get'))) return 'http_get not built (run make -C native/c programs)';
  return false;
}

// Poll for a kernel socket listener on the given port
async function waitForListener(
  kernel: Kernel,
  port: number,
  timeoutMs = 10_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const listener = kernel.socketTable.findListener({ host: '0.0.0.0', port });
    if (listener) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`Timed out waiting for listener on port ${port}`);
}

describe.skipIf(skipReasonNetwork())('cross-runtime network integration', { timeout: 30_000 }, () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it('WasmVM tcp_server <-> Node.js net.connect: data exchange via kernel loopback', async () => {
    const vfs = createInMemoryFileSystem();
    kernel = createKernel({ filesystem: vfs });
    // Mount WasmVM first (provides shell + C programs), then Node
    await kernel.mount(createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }));
    await kernel.mount(createNodeRuntime());

    const PORT = 9090;

    // Start WasmVM TCP server (blocks on accept)
    const serverPromise = kernel.exec(`tcp_server ${PORT}`);

    // Wait for the server to bind+listen in the kernel socket table
    await waitForListener(kernel, PORT);

    // Run Node.js client that connects via net.connect (routes through kernel sockets)
    const clientResult = await kernel.exec(`node -e '
const net = require("net");
const client = net.connect(${PORT}, "127.0.0.1", () => {
  client.write("ping");
});
client.on("data", (data) => {
  console.log("reply:" + data.toString());
  client.end();
});
client.on("end", () => {
  process.exit(0);
});
client.on("error", (err) => {
  console.error("client error:", err.message);
  process.exit(1);
});
'`);

    expect(clientResult.exitCode).toBe(0);
    expect(clientResult.stdout).toContain('reply:pong');

    // Server should also have completed
    const serverResult = await serverPromise;
    expect(serverResult.exitCode).toBe(0);
    expect(serverResult.stdout).toContain(`listening on port ${PORT}`);
    expect(serverResult.stdout).toContain('received: ping');
  });

  it('Node.js http.createServer <-> WasmVM http_get: HTTP via kernel loopback', async () => {
    const vfs = createInMemoryFileSystem();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }));
    await kernel.mount(createNodeRuntime());

    const PORT = 8080;

    // Start Node.js HTTP server that responds with "hello from node"
    const serverProc = kernel.spawn('node', ['-e', `
const http = require("http");
const server = http.createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "text/plain" });
  res.end("hello from node");
});
server.listen(${PORT}, "0.0.0.0", () => {
  console.log("server listening");
});
`], {
      onStdout: () => {},
      onStderr: () => {},
    });

    // Wait for the Node.js server's listener in the kernel socket table
    await waitForListener(kernel, PORT);

    // Run WasmVM http_get client that connects to the Node.js server
    const clientResult = await kernel.exec(`http_get ${PORT}`);

    expect(clientResult.exitCode).toBe(0);
    expect(clientResult.stdout).toContain('body: hello from node');

    // Kill the server process so the test can clean up
    serverProc.kill(15);
    await serverProc.wait();
  });

  it('loopback: neither test touches the host network stack', async () => {
    const vfs = createInMemoryFileSystem();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }));
    await kernel.mount(createNodeRuntime());

    const PORT = 9091;

    // Start WasmVM TCP server
    const serverPromise = kernel.exec(`tcp_server ${PORT}`);
    await waitForListener(kernel, PORT);

    // Connect via kernel socket table directly (test-side client)
    const CLIENT_PID = 999;
    const st = kernel.socketTable;
    const clientId = st.create(AF_INET, SOCK_STREAM, 0, CLIENT_PID);
    await st.connect(clientId, { host: '127.0.0.1', port: PORT });

    // Send data and verify response. All through kernel, no host TCP.
    st.send(clientId, new TextEncoder().encode('ping'));

    let reply = '';
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      const chunk = st.recv(clientId, 256);
      if (chunk && chunk.length > 0) {
        reply += new TextDecoder().decode(chunk);
        break;
      }
      await new Promise((r) => setTimeout(r, 20));
    }

    expect(reply).toBe('pong');

    st.close(clientId, CLIENT_PID);

    const serverResult = await serverPromise;
    expect(serverResult.exitCode).toBe(0);
    expect(serverResult.stdout).toContain('received: ping');
  });
});
