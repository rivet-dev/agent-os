import { createServer } from "node:http";
import { afterEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

const textDecoder = new TextDecoder();

async function runSpawnedProcess(
	vm: AgentOs,
	command: string,
	args: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
	const stdoutChunks: string[] = [];
	const stderrChunks: string[] = [];
	const { pid } = vm.spawn(command, args, {
		onStdout: (chunk) => {
			stdoutChunks.push(textDecoder.decode(chunk));
		},
		onStderr: (chunk) => {
			const decoded = textDecoder.decode(chunk);
			stderrChunks.push(decoded);
		},
	});

	return {
		exitCode: await vm.waitProcess(pid),
		stdout: stdoutChunks.join(""),
		stderr: stderrChunks.join(""),
	};
}

describe("guest http.request transport", () => {
	let vm: AgentOs;

	afterEach(async () => {
		await vm?.dispose();
	});

	test("reaches a guest net listener through the kernel socket path", async () => {
		vm = await AgentOs.create({
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
			},
		});

		const script = [
			'const http = require("node:http");',
			'const net = require("node:net");',
			'const body = JSON.stringify({ ok: true, path: "/transport-check" });',
			"const server = net.createServer((socket) => {",
			'  let buffered = "";',
			'  socket.setEncoding("utf8");',
			'  socket.on("data", (chunk) => {',
			"    buffered += chunk;",
			'    if (!buffered.includes("\\r\\n\\r\\n")) return;',
			"    socket.end([",
			'      "HTTP/1.1 200 OK",',
			'      "Content-Type: application/json",',
			"      `Content-Length: ${Buffer.byteLength(body)}`,",
			'      "Connection: close",',
			'      "",',
			"      body,",
			'    ].join("\\r\\n"));',
			"  });",
			"});",
			'server.listen(0, "127.0.0.1", () => {',
			"  const address = server.address();",
			'  if (!address || typeof address === "string") {',
			'    console.error("missing tcp address");',
			"    process.exit(1);",
			"    return;",
			"  }",
			"  const req = http.get(`http://127.0.0.1:${address.port}/transport-check`, (res) => {",
			'    let responseBody = "";',
			'    res.setEncoding("utf8");',
			'    res.on("data", (chunk) => {',
			"      responseBody += chunk;",
			"    });",
			'    res.on("end", () => {',
			"      console.log(JSON.stringify({ statusCode: res.statusCode, body: responseBody }));",
			"      server.close(() => process.exit(0));",
			"    });",
			"  });",
			'  req.on("error", (error) => {',
			"    console.error(error?.stack ?? String(error));",
			"    server.close(() => process.exit(1));",
			"  });",
			"});",
		].join("\n");

		const result = await runSpawnedProcess(vm, "node", ["-e", script]);

		expect(result.exitCode).toBe(0);
		expect(result.stderr).toBe("");
		expect(JSON.parse(result.stdout.trim())).toEqual({
			statusCode: 200,
			body: JSON.stringify({
				ok: true,
				path: "/transport-check",
			}),
		});
	});

	test("lets a custom HTTP agent set request headers", async () => {
		vm = await AgentOs.create({
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
			},
		});

		const script = [
			'const http = require("node:http");',
			'const { once } = require("node:events");',
			"class HeaderAgent extends http.Agent {",
			"  pendingSocket;",
			"  addRequest(request, options) {",
			'    request.setHeader("x-agent-marker", "present");',
			"    return super.addRequest(request, options);",
			"  }",
			"  async connect(options) {",
			"    const socket = super.createConnection(options);",
			'    if (socket.connecting) await once(socket, "connect");',
			"    return socket;",
			"  }",
			"  createSocket(request, options, callback) {",
			'    request.setHeader("x-create-socket", "used");',
			"    this.connect(options).then((socket) => {",
			"      this.pendingSocket = socket;",
			"      super.createSocket(request, options, callback);",
			"    }, callback);",
			"  }",
			"  createConnection() {",
			"    const socket = this.pendingSocket;",
			"    this.pendingSocket = undefined;",
			'    if (!socket) throw new Error("missing prepared socket");',
			"    return socket;",
			"  }",
			"}",
			"const server = http.createServer();",
			"server.on('request', (request, response) => {",
			'  response.end(`${request.headers["x-agent-marker"] ?? "missing"}:${request.headers["x-create-socket"] ?? "missing"}`);',
			"});",
			'server.listen(0, "127.0.0.1", () => {',
			"  const address = server.address();",
			'  const request = http.get({ hostname: "127.0.0.1", port: address.port, agent: new HeaderAgent() }, (response) => {',
			'    let body = "";',
			'    response.on("data", (chunk) => { body += chunk; });',
			'    response.on("end", () => { console.log(body); server.close(() => process.exit(0)); });',
			"  });",
			'  request.on("error", (error) => { console.error(error?.stack ?? String(error)); server.close(() => process.exit(1)); });',
			"});",
		].join("\n");

		const result = await runSpawnedProcess(vm, "node", ["-e", script]);

		expect(result, result.stderr).toMatchObject({
			exitCode: 0,
			stdout: "present:used\n",
			stderr: "",
		});
	});

	test("fetches from an HTTP server in the same guest process", async () => {
		vm = await AgentOs.create({
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
			},
		});

		const script = [
			'const http = require("node:http");',
			"void (async () => {",
			"const server = http.createServer((request, response) => {",
			"  response.end(`self:${request.url}`);",
			"});",
			'await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });',
			"const address = server.address();",
			"const response = await fetch(`http://127.0.0.1:${address.port}/self-fetch`);",
			"console.log(await response.text());",
			"await new Promise((resolve) => server.close(resolve));",
			"})();",
		].join("\n");

		const result = await runSpawnedProcess(vm, "node", ["-e", script]);

		expect(result, result.stderr).toMatchObject({
			exitCode: 0,
			stdout: "self:/self-fetch\n",
			stderr: "",
		});
	});

	test("buffers a request body until async server middleware attaches listeners", async () => {
		vm = await AgentOs.create({
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
			},
		});

		const script = [
			'const http = require("node:http");',
			"void (async () => {",
			"const server = http.createServer((request, response) => {",
			"  setTimeout(() => {",
			'    let body = "";',
			'    request.setEncoding("utf8");',
			'    request.on("data", (chunk) => { body += chunk; });',
			'    request.on("end", () => response.end(body));',
			"  }, 25);",
			"});",
			'await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });',
			"const address = server.address();",
			'const payload = JSON.stringify({ delayed: true, value: "retained" });',
			"const response = await fetch(`http://127.0.0.1:${address.port}/delayed-body`, {",
			'  method: "POST",',
			'  headers: { "content-type": "application/json" },',
			"  body: payload,",
			"});",
			"console.log(await response.text());",
			"await new Promise((resolve) => server.close(resolve));",
			"})().catch((error) => { console.error(error?.stack ?? String(error)); process.exit(1); });",
		].join("\n");

		const result = await runSpawnedProcess(vm, "node", ["-e", script]);

		expect(result, result.stderr).toMatchObject({
			exitCode: 0,
			stdout: '{"delayed":true,"value":"retained"}\n',
			stderr: "",
		});
	});

	test("keeps a same-process event stream open while fetching a control response", async () => {
		vm = await AgentOs.create({
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
			},
		});

		const script = [
			'const http = require("node:http");',
			"void (async () => {",
			"let eventResponse;",
			"const server = http.createServer((request, response) => {",
			'  if (request.url === "/events") {',
			"    eventResponse = response;",
			'    response.writeHead(200, { "Content-Type": "text/event-stream" });',
			'    response.write("data: ready\\n\\n");',
			"    return;",
			"  }",
			"  response.end(`control:${request.url}`);",
			"});",
			'await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });',
			"const address = server.address();",
			"const abort = new AbortController();",
			"const events = await fetch(`http://127.0.0.1:${address.port}/events`, { signal: abort.signal });",
			"const reader = events.body.getReader();",
			"await reader.read();",
			"const controls = [];",
			"for (let index = 0; index < 5; index++) {",
			"  const response = await fetch(`http://127.0.0.1:${address.port}/control-${index}`);",
			"  controls.push(await response.text());",
			"}",
			"console.log(controls.join(','));",
			"abort.abort();",
			"await reader.cancel().catch(() => {});",
			"eventResponse.destroy();",
			"server.close();",
			"process.exit(0);",
			"})().catch((error) => { console.error(error?.stack ?? String(error)); process.exit(1); });",
		].join("\n");

		const result = await runSpawnedProcess(vm, "node", ["-e", script]);

		expect(result, result.stderr).toMatchObject({
			exitCode: 0,
			stdout:
				"control:/control-0,control:/control-1,control:/control-2,control:/control-3,control:/control-4\n",
			stderr: "",
		});
	});

	test("supports writeFileSync on an fd from a nested guest child", async () => {
		vm = await AgentOs.create({
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
			},
		});

		const childScript = [
			'const fsSync = require("node:fs");',
			'const fs = require("node:fs/promises");',
			"void (async () => {",
			'  await fs.realpath("/workspace/nested-fs/result.txt").catch((error) => {',
			'    if (error?.code !== "ENOENT" && error?.code !== "ENOTDIR") throw error;',
			"  });",
			'  await fs.mkdir("/workspace/nested-fs", { recursive: true });',
			'  const fd = fsSync.openSync("/workspace/nested-fs/session.jsonl", "wx");',
			'  try { fsSync.writeFileSync(fd, "first\\n"); fsSync.writeFileSync(fd, "second\\n"); }',
			"  finally { fsSync.closeSync(fd); }",
			'  await fs.writeFile("/workspace/nested-fs/result.txt", "nested-ok", "utf8");',
			'  console.log(await fs.readFile("/workspace/nested-fs/result.txt", "utf8"));',
			'  console.log((await fs.readFile("/workspace/nested-fs/session.jsonl", "utf8")).trim());',
			"})().catch((error) => { console.error(error?.stack ?? String(error)); process.exit(1); });",
		].join("\n");
		const script = [
			'const { spawn } = require("node:child_process");',
			`const child = spawn("node", ["-e", ${JSON.stringify(childScript)}]);`,
			"child.stdout.on('data', (chunk) => process.stdout.write(chunk));",
			"child.stderr.on('data', (chunk) => process.stderr.write(chunk));",
			"child.on('exit', (code) => process.exit(code ?? 1));",
		].join("\n");
		const result = await runSpawnedProcess(vm, "node", ["-e", script]);

		expect(result, result.stderr).toMatchObject({
			exitCode: 0,
			stdout: "nested-ok\nfirst\nsecond\n",
			stderr: "",
		});
	});

	test("delivers a delayed host response EOF to an unrefed guest socket", async () => {
		const server = createServer((_request, response) => {
			response.writeHead(200, { "Content-Type": "text/event-stream" });
			response.write("data: first\n\n");
			setTimeout(() => response.end("data: done\n\n"), 100);
		});
		await new Promise<void>((resolve) =>
			server.listen(0, "127.0.0.1", resolve),
		);
		const address = server.address();
		if (!address || typeof address === "string") {
			throw new Error("missing host TCP address");
		}

		try {
			vm = await AgentOs.create({
				loopbackExemptPorts: [address.port],
				permissions: {
					fs: "allow",
					network: "allow",
					childProcess: "allow",
				},
			});

			const childScript = [
				'const http = require("node:http");',
				`const request = http.get("http://127.0.0.1:${address.port}/events", (response) => {`,
				'  let body = "";',
				'  response.setEncoding("utf8");',
				'  response.on("data", (chunk) => { body += chunk; });',
				'  response.on("end", () => console.log(JSON.stringify({ body })));',
				"});",
				'request.on("socket", (socket) => socket.unref());',
				'request.on("error", (error) => { console.error(error?.stack ?? String(error)); process.exit(1); });',
			].join("\n");
			const script = [
				'const { spawn } = require("node:child_process");',
				`const child = spawn("node", ["-e", ${JSON.stringify(childScript)}]);`,
				"child.stdout.on('data', (chunk) => process.stdout.write(chunk));",
				"child.stderr.on('data', (chunk) => process.stderr.write(chunk));",
				"const timer = setTimeout(() => { child.kill('SIGKILL'); process.exit(2); }, 3000);",
				"child.on('exit', (code) => { clearTimeout(timer); process.exit(code ?? 1); });",
			].join("\n");
			const result = await runSpawnedProcess(vm, "node", ["-e", script]);

			expect(result, result.stderr).toMatchObject({ exitCode: 0, stderr: "" });
			expect(JSON.parse(result.stdout.trim())).toEqual({
				body: "data: first\n\ndata: done\n\n",
			});
		} finally {
			await new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			});
		}
	});

	test("delivers terminal SSE frames written in one host burst", async () => {
		const frames = [
			"message_start",
			"content_block_start",
			"content_block_delta",
			"content_block_stop",
			"message_delta",
			"message_stop",
		];
		const expectedBody = frames
			.map((type) => `event: ${type}\ndata: ${JSON.stringify({ type })}\n\n`)
			.join("");
		const server = createServer((_request, response) => {
			response.writeHead(200, {
				"Content-Type": "text/event-stream",
				Connection: "keep-alive",
			});
			for (const type of frames) {
				response.write(`event: ${type}\ndata: ${JSON.stringify({ type })}\n\n`);
			}
			response.end();
		});
		await new Promise<void>((resolve) =>
			server.listen(0, "127.0.0.1", resolve),
		);
		const address = server.address();
		if (!address || typeof address === "string") {
			throw new Error("missing host TCP address");
		}

		try {
			vm = await AgentOs.create({
				loopbackExemptPorts: [address.port],
				permissions: {
					fs: "allow",
					network: "allow",
					childProcess: "allow",
				},
			});
			const childScript = [
				"void (async () => {",
				`  const response = await fetch("http://127.0.0.1:${address.port}/events");`,
				"  console.log(JSON.stringify({ body: await response.text() }));",
				"})().catch((error) => { console.error(error?.stack ?? String(error)); process.exit(1); });",
			].join("\n");
			const script = [
				'const { spawn } = require("node:child_process");',
				`const child = spawn("node", ["-e", ${JSON.stringify(childScript)}]);`,
				"child.stdout.on('data', (chunk) => process.stdout.write(chunk));",
				"child.stderr.on('data', (chunk) => process.stderr.write(chunk));",
				"const timer = setTimeout(() => { child.kill('SIGKILL'); process.exit(2); }, 3000);",
				"child.on('exit', (code) => { clearTimeout(timer); process.exit(code ?? 1); });",
			].join("\n");
			const result = await runSpawnedProcess(vm, "node", ["-e", script]);

			expect(result, result.stderr).toMatchObject({ exitCode: 0, stderr: "" });
			expect(JSON.parse(result.stdout.trim())).toEqual({ body: expectedBody });
		} finally {
			await new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			});
		}
	});
});
