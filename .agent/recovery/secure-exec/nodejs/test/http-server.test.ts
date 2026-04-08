/**
 * Tests for http.createServer inside the V8 runtime.
 *
 * Verifies that CJS scripts using http.createServer can listen on a port
 * and respond to HTTP requests routed through the kernel's socket table
 * and HostNetworkAdapter.
 */

import { describe, it, expect, afterEach } from 'vitest';
import { createNodeRuntime } from '../src/kernel-runtime.ts';
import { createNodeHostNetworkAdapter } from '../src/host-network-adapter.ts';
import { createKernel, createInMemoryFileSystem, allowAll } from '@secure-exec/core';
import type { Kernel } from '@secure-exec/core';

describe('http.createServer inside VM', () => {
	let kernel: Kernel;

	afterEach(async () => {
		await kernel?.dispose();
	});

	it('CJS http server listens and responds to requests', async () => {
		kernel = createKernel({
			filesystem: createInMemoryFileSystem(),
			hostNetworkAdapter: createNodeHostNetworkAdapter(),
			permissions: allowAll,
		});
		await kernel.mount(createNodeRuntime());

		const serverScript = `
const http = require("http");
const server = http.createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(JSON.stringify({ status: "ok", method: req.method, url: req.url }));
});
server.listen(0, "0.0.0.0", () => {
  console.log("LISTENING:" + server.address().port);
});
`;
		await kernel.vfs.writeFile('/tmp/server.js', serverScript);

		let resolvePort: (port: number) => void;
		const portPromise = new Promise<number>((resolve) => {
			resolvePort = resolve;
		});

		const proc = kernel.spawn('node', ['/tmp/server.js'], {
			onStdout: (data) => {
				const text = new TextDecoder().decode(data);
				const match = text.match(/LISTENING:(\d+)/);
				if (match) resolvePort(Number(match[1]));
			},
		});

		const port = await portPromise;
		expect(port).toBeGreaterThan(0);

		// Make a request to the server running inside the VM
		const response = await globalThis.fetch(`http://127.0.0.1:${port}/test`);
		expect(response.ok).toBe(true);

		const json = await response.json();
		expect(json).toEqual({
			status: 'ok',
			method: 'GET',
			url: '/test',
		});

		proc.kill();
	}, 30_000);
});
