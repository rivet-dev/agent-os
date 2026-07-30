import http from "node:http";
import net from "node:net";
import { readFileSync } from "node:fs";
import type { BenchmarkOp } from "../lib/layers.js";
import { runGuestProgram, runNodeProgram } from "../lib/layers.js";

const TLS_LOOPBACK_KEY = readFileSync(
	new URL("../../fixtures/tls-loopback-key.pem", import.meta.url),
	"utf8",
);
const TLS_LOOPBACK_CERT = readFileSync(
	new URL("../../fixtures/tls-loopback-cert.pem", import.meta.url),
	"utf8",
);
const TLS_LOOPBACK_BODY = "hello-loopback-tls";
const EXTERNAL_HTTP_BODY = "external-host-http-ok";
const EXTERNAL_TCP_PAYLOAD = "external-tcp-echo";
const UDP_BIG_BYTES = 60 * 1024;

async function closeServer(server: http.Server | net.Server): Promise<void> {
	await new Promise<void>((resolve, reject) => {
		server.close((error?: Error) => {
			if (error && (error as NodeJS.ErrnoException).code !== "ERR_SERVER_NOT_RUNNING") {
				reject(error);
				return;
			}
			resolve();
		});
	});
}

async function listenHttpExternalServer(): Promise<{ port: number; server: http.Server }> {
	const server = http.createServer((_req, res) => {
		res.setHeader("connection", "close");
		res.end(EXTERNAL_HTTP_BODY);
	});
	await new Promise<void>((resolve, reject) => {
		server.on("error", reject);
		server.listen(0, "127.0.0.1", resolve);
	});
	const address = server.address();
	if (!address || typeof address === "string") {
		await closeServer(server);
		throw new Error("http external server did not bind to a TCP port");
	}
	return { port: address.port, server };
}

async function listenTcpExternalServer(): Promise<{ port: number; server: net.Server }> {
	const server = net.createServer((socket) => {
		socket.on("data", (data) => socket.end(data));
	});
	await new Promise<void>((resolve, reject) => {
		server.on("error", reject);
		server.listen(0, "127.0.0.1", resolve);
	});
	const address = server.address();
	if (!address || typeof address === "string") {
		await closeServer(server);
		throw new Error("tcp external server did not bind to a TCP port");
	}
	return { port: address.port, server };
}

async function withHttpExternalServer<T>(
	callback: (port: number) => Promise<T> | T,
): Promise<T> {
	const { port, server } = await listenHttpExternalServer();
	try {
		return await callback(port);
	} finally {
		await closeServer(server);
	}
}

async function withTcpExternalServer<T>(
	callback: (port: number) => Promise<T> | T,
): Promise<T> {
	const { port, server } = await listenTcpExternalServer();
	try {
		return await callback(port);
	} finally {
		await closeServer(server);
	}
}

async function runHttpExternalClient(port: number, iters: number, warmup: number): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = Number(process.hrtime.bigint()) / 1e6;
		const body = await new Promise<string>((resolve, reject) => {
			const req = http.get(
				{
					agent: false,
					host: "127.0.0.1",
					port,
					path: "/",
					headers: { connection: "close" },
				},
				(res) => {
					const chunks: Buffer[] = [];
					res.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
					res.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
				},
			);
			req.on("error", reject);
		});
		if (body !== EXTERNAL_HTTP_BODY) {
			throw new Error(`bad external HTTP body: ${JSON.stringify(body)}`);
		}
		const ms = Number(process.hrtime.bigint()) / 1e6 - start;
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function runTcpExternalClient(port: number, iters: number, warmup: number): Promise<number[]> {
	const samples: number[] = [];
	const expected = Buffer.from(EXTERNAL_TCP_PAYLOAD);
	for (let i = 0; i < warmup + iters; i++) {
		const start = Number(process.hrtime.bigint()) / 1e6;
		const body = await new Promise<Buffer>((resolve, reject) => {
			const socket = net.connect(port, "127.0.0.1");
			const chunks: Buffer[] = [];
			socket.on("connect", () => socket.write(expected));
			socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
			socket.on("error", reject);
			socket.on("close", () => resolve(Buffer.concat(chunks)));
		});
		if (!body.equals(expected)) {
			throw new Error(`bad external TCP echo: ${body.toString("hex")}`);
		}
		const ms = Number(process.hrtime.bigint()) / 1e6 - start;
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

function httpExternalGetProgram(port: number): string {
	return `
import http from "node:http";

const iters = Number(process.env.BENCH_ITERATIONS || 20);
const warmup = Number(process.env.BENCH_WARMUP || 5);
const expectedBody = ${JSON.stringify(EXTERNAL_HTTP_BODY)};
const port = ${port};
const samples = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;

async function once() {
  const body = await new Promise((resolve, reject) => {
    const req = http.get({
      agent: false,
      host: "127.0.0.1",
      port,
      path: "/",
      headers: { connection: "close" },
    }, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
      res.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    });
    req.on("error", reject);
  });
  if (body !== expectedBody) {
    throw new Error("bad external HTTP body: " + JSON.stringify(body));
  }
}

for (let i = 0; i < warmup + iters; i++) {
  const start = now();
  await once();
  const ms = now() - start;
  if (i >= warmup) samples.push(ms);
}
process.stdout.write(JSON.stringify({ samples }));
`;
}

function tcpExternalEchoProgram(port: number): string {
	return `
import net from "node:net";

const iters = Number(process.env.BENCH_ITERATIONS || 20);
const warmup = Number(process.env.BENCH_WARMUP || 5);
const expected = Buffer.from(${JSON.stringify(EXTERNAL_TCP_PAYLOAD)});
const port = ${port};
const samples = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;

async function once() {
  const body = await new Promise((resolve, reject) => {
    const socket = net.connect(port, "127.0.0.1");
    const chunks = [];
    socket.on("connect", () => socket.write(expected));
    socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
    socket.on("error", reject);
    socket.on("close", () => resolve(Buffer.concat(chunks)));
  });
  if (!Buffer.from(body).equals(expected)) {
    throw new Error("bad external TCP echo: " + Buffer.from(body).toString("hex"));
  }
}

for (let i = 0; i < warmup + iters; i++) {
  const start = now();
  await once();
  const ms = now() - start;
  if (i >= warmup) samples.push(ms);
}
process.stdout.write(JSON.stringify({ samples }));
`;
}

function tlsLoopbackGetProgram(): string {
	return `
import https from "node:https";

const iters = Number(process.env.BENCH_ITERATIONS || 20);
const warmup = Number(process.env.BENCH_WARMUP || 5);
const key = ${JSON.stringify(TLS_LOOPBACK_KEY)};
const cert = ${JSON.stringify(TLS_LOOPBACK_CERT)};
	const expectedBody = ${JSON.stringify(TLS_LOOPBACK_BODY)};
	const port = 18443;
	const samples = [];
const now = () => Number(process.hrtime.bigint()) / 1e6;
const serverSockets = new Set();

const server = https.createServer({ key, cert }, (_req, res) => {
  res.setHeader("connection", "close");
  res.end(expectedBody);
});
server.on("connection", (socket) => {
  serverSockets.add(socket);
  socket.on("close", () => serverSockets.delete(socket));
});

await new Promise((resolve, reject) => {
  server.on("error", reject);
  server.listen(port, "127.0.0.1", resolve);
});

async function once(iteration) {
  const body = await new Promise((resolve, reject) => {
    let response = "";
    const req = https.get({
      agent: false,
      host: "127.0.0.1",
      localPort: 30000 + iteration,
      port,
      path: "/",
      rejectUnauthorized: false,
    }, (res) => {
      res.setEncoding("utf8");
      res.on("data", (chunk) => {
        response += chunk;
      });
      res.on("end", () => {
        const socket = res.socket;
        socket?.destroy();
        req.destroy();
        resolve(response);
      });
    });
    req.on("error", reject);
  });
  if (body !== expectedBody) {
    throw new Error("bad TLS loopback body: " + JSON.stringify(body));
  }
}

try {
	  for (let i = 0; i < warmup + iters; i++) {
	    const start = now();
	    await once(i);
	    const ms = now() - start;
	    if (i >= warmup) samples.push(ms);
	  }
  for (const socket of serverSockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
  process.stdout.write(JSON.stringify({ samples }));
} catch (error) {
  for (const socket of serverSockets) socket.destroy();
  await new Promise((resolve) => server.close(resolve));
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
}
`;
}

function udpEchoOp(name: string, sizeBytes: number): BenchmarkOp {
		return {
			family: "net",
			name,
			nativeOp: "udp_echo",
			nativeArgs: ["--size-bytes", String(sizeBytes)],
			wasmUnsupportedReason: "UDP sockets are not supported in the native-baseline wasm lane",
			fileLine: "crates/sidecar/src/execution.rs:2712",
		reproducer: `node:dgram udp4 socket sends one ${sizeBytes} byte datagram to its own loopback address inside VM`,
		program: `async () => {
  const dgram = await import("node:dgram");
  const payload = Buffer.alloc(${sizeBytes}, 7);
  const createSocket = dgram.createSocket ?? dgram.default?.createSocket;
  if (typeof createSocket !== "function") throw new Error("dgram.createSocket is not a function");
  await new Promise((resolve, reject) => {
    const socket = createSocket("udp4");
    socket.on("error", (error) => {
      socket.close();
      reject(error);
    });
    socket.on("message", (message) => {
      socket.close(() => message.equals(payload) ? resolve() : reject(new Error("bad udp echo: " + message.length)));
    });
    socket.bind(0, "127.0.0.1", () => {
      const address = socket.address();
      socket.send(payload, address.port, "127.0.0.1");
    });
  });
}`,
	};
}

function unixEchoOp(name: string, sizeBytes: number): BenchmarkOp {
		return {
			family: "net",
			name,
			nativeOp: "unix_echo",
			nativeArgs: ["--size-bytes", String(sizeBytes)],
			wasmUnsupportedReason: "Unix-domain sockets are not supported in the native-baseline wasm lane",
			fileLine: "crates/sidecar/src/execution.rs:2237",
		reproducer: `Unix-domain socket echo one ${sizeBytes} byte payload inside VM`,
		program: `async () => {
  const fs = await import("node:fs");
  const net = await import("node:net");
  const os = await import("node:os");
  const path = await import("node:path");
  const payload = Buffer.alloc(${sizeBytes}, 7);
  const sock = path.join(
    os.tmpdir(),
    "fuzz-perf-unix-echo-" + process.pid + "-" + Math.random().toString(16).slice(2) + ".sock",
  );
  await new Promise((resolve, reject) => {
    const server = net.createServer((socket) => {
      const chunks = [];
      socket.on("data", (data) => {
        chunks.push(data);
        const got = Buffer.concat(chunks);
        if (got.length >= payload.length) socket.end(got);
      });
    });
    const cleanup = () => {
      try { fs.unlinkSync(sock); } catch {}
    };
    server.on("error", (error) => {
      cleanup();
      reject(error);
    });
    server.listen(sock, () => {
      const client = net.connect(sock);
      const chunks = [];
      client.on("data", (data) => chunks.push(data));
      client.on("error", reject);
      client.on("close", () => {
        const got = Buffer.concat(chunks);
        server.close(() => {
          cleanup();
          got.equals(payload) ? resolve() : reject(new Error("bad unix echo: " + got.length));
        });
      });
      client.write(payload);
    });
  });
}`,
	};
}

function tcpEchoOp(name: string, sizeBytes: number, nativeOp: "tcp_echo" | "tcp_throughput"): BenchmarkOp {
		return {
			family: "net",
			name,
			nativeOp,
			nativeArgs: ["--size-bytes", String(sizeBytes)],
			wasmUnsupportedReason: "TCP sockets are not supported in the native-baseline wasm lane",
			fileLine: "crates/kernel/src/socket_table.rs:1413",
		reproducer: `localhost TCP echo of one ${sizeBytes} byte payload inside VM`,
		program: `async () => {
  const net = await import("node:net");
  const payload = Buffer.alloc(${sizeBytes}, 7);
  await new Promise((resolve, reject) => {
    const server = net.createServer((socket) => {
      const chunks = [];
      socket.on("data", (d) => {
        chunks.push(d);
        const got = Buffer.concat(chunks);
        if (got.length >= payload.length) socket.end(got);
      });
    });
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      const socket = net.connect(port, "127.0.0.1");
      const chunks = [];
      socket.on("data", (d) => chunks.push(d));
      socket.on("error", reject);
      socket.on("close", () => {
        const got = Buffer.concat(chunks);
        server.close(() => got.equals(payload) ? resolve() : reject(new Error("short echo: " + got.length)));
      });
      socket.write(payload);
    });
  });
}`,
	};
}

export const netFamily: BenchmarkOp[] = [
	udpEchoOp("udp_echo_small", 16),
	udpEchoOp("udp_echo_big", UDP_BIG_BYTES),
	unixEchoOp("unix_echo_small", 16),
	unixEchoOp("unix_echo_big", 64 * 1024),
		{
			family: "net",
			name: "http_loopback_get",
			nativeOp: "http_loopback_get",
			wasmUnsupportedReason: "TCP HTTP loopback is not supported in the native-baseline wasm lane",
			fileLine: "crates/v8-runtime/src/asset_cache.rs:4750",
		reproducer: "node:http loopback GET inside VM",
		program: `async () => {
  const http = await import("node:http");
  const server = http.createServer((_req, res) => {
    res.end("ok");
  });
  await new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      http.get({ hostname: "127.0.0.1", port, path: "/" }, (res) => {
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => {
          const body = Buffer.concat(chunks).toString("utf8");
          server.close(() => body === "ok" ? resolve() : reject(new Error("bad http body")));
        });
      }).on("error", (error) => {
        server.close(() => reject(error));
      });
    });
	  });
	}`,
	},
		{
			family: "net",
			name: "http_external_get",
			nativeUnsupportedReason: "external-loopback row depends on a Node harness listener, no native-baseline analogue",
			wasmUnsupportedReason: "host-side benchmark listener requires Node harness networking",
		fileLine: "crates/sidecar/src/execution.rs:13919",
		reproducer: "node:http GET from guest to a host-side loopback-exempt HTTP server",
		runNode: (iters, warmup) =>
			withHttpExternalServer((port) => runHttpExternalClient(port, iters, warmup)),
		prepareVm: async () => {
			const { port, server } = await listenHttpExternalServer();
			return {
				options: { loopbackExemptPorts: [port] },
				context: port,
				cleanup: () => closeServer(server),
			};
		},
		runGuest: (vm, iters, warmup, context) =>
			runGuestProgram(
				vm,
				httpExternalGetProgram(assertPortContext(context, "http_external_get")),
				iters,
				warmup,
				"net-http-external-get",
			),
	},
		{
			family: "net",
			name: "http2_loopback_get",
			nativeUnsupportedReason: "HTTP/2 is a JS-runtime protocol surface in this matrix",
			wasmUnsupportedReason: "HTTP/2 unsupported in wasm baseline",
		fileLine: "packages/build-tools/bridge-src/builtins/http2.ts:1472",
		reproducer: "node:http2 h2c loopback GET inside VM",
		program: `async () => {
  const http2 = await import("node:http2");
  const server = http2.createServer();
  server.on("stream", (stream, headers) => {
    if (headers[":path"] !== "/") {
      stream.respond({ ":status": 404 });
      stream.end("missing");
      return;
    }
    stream.respond({ ":status": 200, "content-type": "text/plain" });
    stream.end("ok-h2");
  });
  await new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      const session = http2.connect("http://127.0.0.1:" + port);
      const req = session.request({ ":path": "/" });
      const chunks = [];
      session.on("error", (error) => {
        server.close(() => reject(error));
      });
      req.on("data", (chunk) => chunks.push(chunk));
      req.on("end", () => {
        const body = Buffer.concat(chunks).toString("utf8");
        session.close();
        server.close(() => body === "ok-h2" ? resolve() : reject(new Error("bad http2 body")));
      });
      req.on("error", (error) => {
        session.close();
        server.close(() => reject(error));
      });
      req.end();
    });
  });
}`,
	},
		{
			family: "net",
			name: "fetch_loopback_get",
			nativeUnsupportedReason: "fetch is a JS-runtime undici surface",
			wasmUnsupportedReason: "fetch is a JS-runtime undici surface",
			fileLine: "crates/v8-runtime/src/asset_cache.rs:4750",
		reproducer: "global fetch loopback GET inside VM",
		program: `async () => {
  if (typeof fetch !== "function") throw new Error("fetch is not defined");
  const http = await import("node:http");
  const server = http.createServer((_req, res) => {
    res.end("ok");
  });
  await new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", async () => {
      try {
        const port = server.address().port;
        const res = await fetch("http://127.0.0.1:" + port + "/");
        const body = await res.text();
        server.close(() => body === "ok" ? resolve() : reject(new Error("bad fetch body")));
      } catch (error) {
        server.close(() => reject(error));
      }
    });
  });
}`,
	},
		{
			family: "net",
			name: "tls_loopback_get",
			nativeUnsupportedReason: "TLS is a JS-runtime protocol surface in this matrix",
			wasmUnsupportedReason: "TLS unsupported in wasm baseline",
		fileLine: "crates/sidecar/src/execution.rs:13532",
		reproducer: "persistent node:https loopback server; each iteration opens a fresh https.get connection inside VM",
		runNode: (iters, warmup) => runNodeProgram(tlsLoopbackGetProgram(), iters, warmup),
		runGuest: (vm, iters, warmup) =>
			runGuestProgram(vm, tlsLoopbackGetProgram(), iters, warmup, "net-tls-loopback-get"),
	},
		{
			family: "net",
			name: "tcp_connect_close",
			nativeOp: "tcp_connect",
			wasmUnsupportedReason: "TCP sockets are not supported in the native-baseline wasm lane",
			fileLine: "crates/kernel/src/socket_table.rs:382",
		reproducer: "node net.createServer(); net.connect(port).end() inside VM",
		program: `async () => {
  const net = await import("node:net");
  await new Promise((resolve, reject) => {
    const server = net.createServer((socket) => socket.end());
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      const socket = net.connect(port, "127.0.0.1");
      socket.on("error", reject);
      socket.on("close", () => server.close(resolve));
      socket.end();
    });
  });
}`,
	},
	tcpEchoOp("tcp_echo_small", 16, "tcp_echo"),
		{
			family: "net",
			name: "tcp_external_echo",
			nativeOp: "tcp_echo",
		wasmUnsupportedReason: "host-side benchmark listener requires Node harness networking",
		fileLine: "crates/sidecar/src/execution.rs:13919",
		reproducer: "guest net.connect to a host-side loopback-exempt TCP echo server, one 16-byte echo",
		runNode: (iters, warmup) =>
			withTcpExternalServer((port) => runTcpExternalClient(port, iters, warmup)),
		prepareVm: async () => {
			const { port, server } = await listenTcpExternalServer();
			return {
				options: { loopbackExemptPorts: [port] },
				context: port,
				cleanup: () => closeServer(server),
			};
		},
		runGuest: (vm, iters, warmup, context) =>
			runGuestProgram(
				vm,
				tcpExternalEchoProgram(assertPortContext(context, "tcp_external_echo")),
				iters,
				warmup,
				"net-tcp-external-echo",
			),
	},
		{
			family: "net",
			name: "tcp_concurrent_4",
			nativeOp: "tcp_concurrent",
			wasmUnsupportedReason: "TCP sockets are not supported in the native-baseline wasm lane",
			fileLine: "crates/kernel/src/socket_table.rs:382",
		reproducer: "four concurrent localhost TCP clients connect to one VM server",
		program: `async () => {
  const net = await import("node:net");
  await new Promise((resolve, reject) => {
    let accepted = 0;
    const server = net.createServer((socket) => {
      socket.on("data", () => socket.end());
      if (++accepted === 4) server.close(resolve);
    });
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      for (let i = 0; i < 4; i++) {
        const socket = net.connect(port, "127.0.0.1");
        socket.on("error", reject);
        socket.write("x");
      }
    });
  });
}`,
	},
	tcpEchoOp("tcp_echo_big", 64 * 1024, "tcp_throughput"),
	{
		// Measures write count/cadence, not payload-size scaling; keep the count suffix.
			family: "net",
			name: "tcp_tiny_writes_16",
			nativeOp: "tcp_tiny_writes",
			wasmUnsupportedReason: "TCP sockets are not supported in the native-baseline wasm lane",
			fileLine: "crates/kernel/src/socket_table.rs:1335",
		reproducer: "localhost TCP echo using sixteen one-byte writes inside VM",
		program: `async () => {
  const net = await import("node:net");
  await new Promise((resolve, reject) => {
    const server = net.createServer((socket) => {
      const chunks = [];
      socket.on("data", (d) => {
        chunks.push(d);
        if (Buffer.concat(chunks).length >= 16) socket.end(Buffer.concat(chunks));
      });
    });
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      const socket = net.connect(port, "127.0.0.1");
      const chunks = [];
      socket.on("data", (d) => chunks.push(d));
      socket.on("error", reject);
      socket.on("close", () => {
        const got = Buffer.concat(chunks);
        server.close(() => got.length === 16 ? resolve() : reject(new Error("short tiny echo")));
      });
      for (let i = 0; i < 16; i++) socket.write("x");
    });
  });
}`,
	},
];

function assertPortContext(context: unknown, op: string): number {
	if (typeof context !== "number") {
		throw new Error(`${op} missing prepared loopback port`);
	}
	return context;
}
