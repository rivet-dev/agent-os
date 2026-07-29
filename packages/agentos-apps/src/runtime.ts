import { createHash } from "node:crypto";
import { posix } from "node:path";
import { controlFetch } from "./control-request.js";

const MAX_FILE_PATH_BYTES = 1_024;
const MAX_ENGINE_RESPONSE_BYTES = 1024 * 1024;
const MAX_ENGINE_DATACENTERS = 128;
const DEFAULT_SERVERLESS_REQUEST_LIFESPAN_SECONDS = 60 * 60;
const DEFAULT_SERVERLESS_MAX_RUNNERS = 1_024;
const DEFAULT_SERVERLESS_METADATA_POLL_INTERVAL_MS = 1_000;

export const APP_CALLBACK_SECRET_HEADER = "x-agentos-app-callback-token";

/** Stable per-app pool so several apps can share one Rivet namespace safely. */
export function appRunnerPool(appId: string): string {
	const suffix = createHash("sha256").update(appId).digest("hex").slice(0, 16);
	return `agentos-apps-${suffix}`;
}

async function readBoundedText(response: Response): Promise<string> {
	if (!response.body) return "";
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let bytes = 0;
	try {
		for (;;) {
			const { value, done } = await reader.read();
			if (done) break;
			bytes += value.byteLength;
			if (bytes > MAX_ENGINE_RESPONSE_BYTES) {
				await reader.cancel();
				throw new RangeError(
					`Rivet Engine response exceeded ${MAX_ENGINE_RESPONSE_BYTES} bytes`,
				);
			}
			chunks.push(value);
		}
	} finally {
		reader.releaseLock();
	}
	return new TextDecoder().decode(Buffer.concat(chunks));
}

function engineUrl(endpoint: string, path: string[], namespace: string): URL {
	const url = new URL(endpoint);
	url.pathname = `/${path.map(encodeURIComponent).join("/")}`;
	url.search = "";
	url.searchParams.set("namespace", namespace);
	return url;
}

/** Idempotently points a guest namespace's serverless pool at its Apps actor. */
export async function ensureServerlessRunnerConfig(input: {
	endpoint: string;
	namespace: string;
	url: string;
	pool: string;
	token?: string;
	callbackSecret: string;
}): Promise<void> {
	const headers = {
		accept: "application/json",
		"content-type": "application/json",
		...(input.token ? { authorization: `Bearer ${input.token}` } : {}),
	};
	const datacentersResponse = await controlFetch(
		engineUrl(input.endpoint, ["datacenters"], input.namespace),
		{ headers },
	);
	const datacentersText = await readBoundedText(datacentersResponse);
	if (!datacentersResponse.ok) {
		throw new Error(
			`Rivet datacenter lookup failed with HTTP ${datacentersResponse.status}: ${datacentersText}`,
		);
	}
	const parsed = JSON.parse(datacentersText) as {
		datacenters?: Array<{ name?: unknown }>;
	};
	if (
		!Array.isArray(parsed.datacenters) ||
		parsed.datacenters.length === 0 ||
		parsed.datacenters.length > MAX_ENGINE_DATACENTERS
	) {
		throw new Error(
			`Rivet datacenter lookup returned an invalid count; expected 1-${MAX_ENGINE_DATACENTERS}`,
		);
	}
	const datacenters: Record<string, unknown> = {};
	for (const datacenter of parsed.datacenters) {
		if (typeof datacenter.name !== "string" || datacenter.name.length === 0) {
			throw new Error("Rivet datacenter lookup returned an invalid name");
		}
		datacenters[datacenter.name] = {
			serverless: {
				url: input.url,
				headers: {
					[APP_CALLBACK_SECRET_HEADER]: input.callbackSecret,
				},
				request_lifespan: DEFAULT_SERVERLESS_REQUEST_LIFESPAN_SECONDS,
				metadata_poll_interval: DEFAULT_SERVERLESS_METADATA_POLL_INTERVAL_MS,
				max_runners: DEFAULT_SERVERLESS_MAX_RUNNERS,
				min_runners: 0,
				runners_margin: 0,
				slots_per_runner: 1,
			},
			metadata: {},
			drain_on_version_upgrade: true,
		};
	}

	const response = await controlFetch(
		engineUrl(input.endpoint, ["runner-configs", input.pool], input.namespace),
		{
			method: "PUT",
			headers,
			body: JSON.stringify({ datacenters }),
		},
	);
	const responseText = await readBoundedText(response);
	if (!response.ok) {
		throw new Error(
			`Rivet runner config upsert failed with HTTP ${response.status}: ${responseText}`,
		);
	}
}

export function normalizeAppPath(input: string): string {
	if (typeof input !== "string" || input.length === 0 || input.includes("\0")) {
		throw new Error(
			"application file paths must be non-empty strings without NUL bytes",
		);
	}
	const normalized = posix.normalize(`/${input}`).slice(1);
	if (
		input.startsWith("/") ||
		input.split("/").includes("..") ||
		normalized === "" ||
		normalized === "." ||
		normalized === ".." ||
		normalized.startsWith("../") ||
		Buffer.byteLength(normalized) > MAX_FILE_PATH_BYTES
	) {
		throw new Error(
			`application file path escapes its root: ${JSON.stringify(input)}`,
		);
	}
	return normalized;
}

export function canonicalDeploymentHash(input: {
	files: Record<string, Uint8Array>;
	entrypoint: string;
	build: boolean;
	staticRoot?: string;
	packagingIdentity: string;
	deploymentIdentity?: string;
}): string {
	const hash = createHash("sha256");
	// The release hash covers generated runner semantics as well as user input.
	// Bump this tag whenever runnerSource changes so an existing deployment cannot
	// reuse an artifact built with an older host adapter.
	hash.update("agentos-apps-release-v15\0");
	const field = (value: string | Uint8Array) => {
		const bytes = typeof value === "string" ? Buffer.from(value) : value;
		const length = Buffer.allocUnsafe(8);
		length.writeBigUInt64BE(BigInt(bytes.byteLength));
		hash.update(length);
		hash.update(bytes);
	};
	for (const [path, content] of Object.entries(input.files).sort(([a], [b]) =>
		a < b ? -1 : a > b ? 1 : 0,
	)) {
		field(normalizeAppPath(path));
		field(content);
	}
	field(normalizeAppPath(input.entrypoint));
	field(JSON.stringify({ build: input.build, staticRoot: input.staticRoot }));
	field(input.packagingIdentity);
	field(input.deploymentIdentity ?? "");
	return hash.digest("hex");
}

export function releaseEnvoyVersion(release: string): number {
	const version = Number.parseInt(release.slice(0, 8), 16) & 0x7fffffff;
	return version || 1;
}

export function runtimeLoopbackPort(
	endpoint: string | undefined,
): number | undefined {
	if (!endpoint) return undefined;
	const url = new URL(endpoint);
	if (url.protocol !== "http:" && url.protocol !== "https:") {
		throw new TypeError(`unsupported Rivet endpoint protocol: ${url.protocol}`);
	}
	const hostname = url.hostname.toLowerCase();
	const loopback =
		hostname === "localhost" ||
		hostname === "[::1]" ||
		hostname === "::1" ||
		/^127(?:\.|$)/.test(hostname);
	if (!loopback) return undefined;
	if (url.port) return Number.parseInt(url.port, 10);
	return url.protocol === "http:" ? 80 : 443;
}

export function runnerSource(input: {
	entrypoint: string;
	release: string;
	port: number;
	maxRequestBytes: number;
	maxResponseBytes: number;
	usesRivetKit: boolean;
}): string {
	const entrypoint = `./${normalizeAppPath(input.entrypoint)}`;
	const rivetKitImport = input.usesRivetKit
		? 'import { createRequire } from "node:module";'
		: "";
	const rivetKitBootstrap = input.usesRivetKit
		? `// AgentOS applies per-process environment state before evaluating this module's
// body. Initialize RivetKit's wasm-bindgen module from bytes because Node fetch
// does not support the file: URL used by wasm-bindgen's default initializer.
const [
  { default: initializeRivetKit },
  { readFile },
  { Registry },
] =
  await Promise.all([
    import("@rivetkit/rivetkit-wasm"),
    import("node:fs/promises"),
    import("rivetkit"),
  ]);
const wasmPath =
  typeof __AGENTOS_RIVETKIT_WASM_PATH__ === "string"
    ? new URL(__AGENTOS_RIVETKIT_WASM_PATH__, import.meta.url)
    : createRequire(import.meta.url).resolve(
        "@rivetkit/rivetkit-wasm/rivetkit_wasm_bg.wasm",
      );
await initializeRivetKit({
  module_or_path: await readFile(wasmPath),
});
const originalStart = Registry.prototype.start;
Registry.prototype.start = function agentOSManagedStart() {};
let appModule;
try {
  appModule = await import(${JSON.stringify(entrypoint)});
} finally {
  Registry.prototype.start = originalStart;
}
const guestRegistry = appModule.registry;
if (typeof guestRegistry?.handler !== "function") {
  throw new TypeError(
    "AgentOS App using RivetKit must export const registry = setup(...)",
  );
}`
		: `const appModule = await import(${JSON.stringify(entrypoint)});
const guestRegistry = undefined;`;
	return `import http from "node:http";
${rivetKitImport}

const release = ${JSON.stringify(input.release)};
const port = ${input.port};
const maxRequestBytes = ${input.maxRequestBytes};
const maxResponseBytes = ${input.maxResponseBytes};
${rivetKitBootstrap}
const exported = appModule.fetch ?? appModule.default?.fetch ?? appModule.default;
const appFetch = typeof exported === "function"
  ? exported
  : typeof exported?.fetch === "function"
    ? exported.fetch.bind(exported)
    : undefined;
if (!appFetch) {
  throw new TypeError("AgentOS App entrypoint must export fetch(request) or a default fetch handler");
}

async function dispatchRequest(request) {
  const response = guestRegistry !== undefined &&
    new URL(request.url).pathname.startsWith("/api/rivet")
    ? await guestRegistry.handler(request)
    : await appFetch(request);
  if (!(response instanceof Response)) {
    throw new TypeError("AgentOS App fetch handler must return a Response");
  }
  return response;
}

// Rivet Engine callbacks may cause the guest WASM runtime to call back into the
// Engine before returning. Keep that nested path off the VM HTTP request lane,
// which is intentionally waiting for response headers. The line protocol is
// streaming because RivetKit's /start response is a long-lived SSE body.
const rpcPrefix = "AGENTOS_APPS_RPC ";
const rpcRequests = new Map();
let stdinBuffer = "";
async function writeRpc(message) {
  if (!process.stdout.write(rpcPrefix + JSON.stringify(message) + "\\n")) {
    await new Promise((resolve) => process.stdout.once("drain", resolve));
  }
}
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  stdinBuffer += chunk;
  for (;;) {
    const newline = stdinBuffer.indexOf("\\n");
    if (newline < 0) break;
    const line = stdinBuffer.slice(0, newline);
    stdinBuffer = stdinBuffer.slice(newline + 1);
    if (!line) continue;
    void (async () => {
      let id = "unknown";
      try {
        const message = JSON.parse(line);
        id = String(message.id);
        if (message.event === "cancel") {
          const rpc = rpcRequests.get(id);
          rpc?.abort.abort(new Error("host cancelled guest request"));
          rpc?.ack?.();
          return;
        }
        if (message.event === "ack") {
          const rpc = rpcRequests.get(id);
          rpc?.ack?.();
          if (rpc) rpc.ack = undefined;
          return;
        }
        const abort = new AbortController();
        const rpc = { abort, ack: undefined };
        rpcRequests.set(id, rpc);
        const body = message.bodyBase64
          ? Buffer.from(message.bodyBase64, "base64")
          : undefined;
        const request = new Request(message.url, {
          method: message.method,
          headers: message.headers,
          body: message.method === "GET" || message.method === "HEAD"
            ? undefined
            : body,
          signal: abort.signal,
        });
        const response = await dispatchRequest(request);
        const headers = [];
        response.headers.forEach((value, name) => {
          if (name !== "set-cookie") headers.push([name, value]);
        });
        for (const cookie of response.headers.getSetCookie?.() ?? []) {
          headers.push(["set-cookie", cookie]);
        }
        await writeRpc({
          id,
          event: "head",
          status: response.status,
          statusText: response.statusText,
          headers,
        });
        if (response.body) {
          for await (const chunk of response.body) {
            if (chunk.byteLength > maxResponseBytes) {
              throw new RangeError("Response chunk exceeds AgentOS Apps limit");
            }
            const acknowledged = new Promise((resolve) => {
              rpc.ack = resolve;
            });
            await writeRpc({
              id,
              event: "chunk",
              bodyBase64: Buffer.from(chunk).toString("base64"),
            });
            await acknowledged;
            if (abort.signal.aborted) return;
          }
        }
        await writeRpc({ id, event: "end" });
      } catch (error) {
        if (rpcRequests.get(id)?.abort.signal.aborted) return;
        console.error("AgentOS App RPC request failed", error);
        await writeRpc({
          id,
          event: "error",
          status: 500,
          statusText: "Internal Server Error",
          headers: [["content-type", "text/plain; charset=utf-8"]],
          message: "Internal Server Error",
        });
      } finally {
        rpcRequests.delete(id);
      }
    })();
  }
});

http.createServer(async (incoming, outgoing) => {
  const abort = new AbortController();
  const cancel = () => abort.abort(new Error("client disconnected"));
  incoming.once("aborted", cancel);
  outgoing.once("close", () => {
    if (!outgoing.writableEnded) cancel();
  });
  try {
    if (incoming.url === "/.agentos/ready") {
      outgoing.setHeader("content-type", "application/json");
      outgoing.end(JSON.stringify({ release }));
      return;
    }

    const body = await new Promise((resolve, reject) => {
      const chunks = [];
      let requestBytes = 0;
      let tooLarge = false;
      incoming.on("data", (chunk) => {
        requestBytes += chunk.byteLength;
        if (requestBytes > maxRequestBytes) {
          tooLarge = true;
          chunks.length = 0;
          return;
        }
        if (!tooLarge) chunks.push(Buffer.from(chunk));
      });
      incoming.once("end", () => resolve({
        tooLarge,
        bytes: tooLarge ? undefined : Buffer.concat(chunks),
      }));
      incoming.once("error", reject);
    });
    if (body.tooLarge) {
      outgoing.writeHead(413);
      outgoing.end("Request body exceeds AgentOS Apps limit");
      return;
    }

    const origin = "http://" + (incoming.headers.host ?? "agentos-app");
    const request = new Request(new URL(incoming.url ?? "/", origin), {
      method: incoming.method,
      headers: incoming.headers,
      body: incoming.method === "GET" || incoming.method === "HEAD"
        ? undefined
        : body.bytes,
      signal: abort.signal,
    });
    const response = await dispatchRequest(request);

    outgoing.statusCode = response.status;
    outgoing.statusMessage = response.statusText;
    const setCookies = response.headers.getSetCookie?.() ?? [];
    response.headers.forEach((value, name) => {
      if (name !== "set-cookie") outgoing.setHeader(name, value);
    });
    if (setCookies.length > 0) outgoing.setHeader("set-cookie", setCookies);
    outgoing.flushHeaders?.();
    if (!response.body) {
      outgoing.end();
      return;
    }

    let responseBytes = 0;
    for await (const chunk of response.body) {
      responseBytes += chunk.byteLength;
      if (responseBytes > maxResponseBytes) {
        throw new RangeError("Response body exceeds AgentOS Apps limit");
      }
      if (!outgoing.write(Buffer.from(chunk))) {
        await new Promise((resolve) => outgoing.once("drain", resolve));
      }
    }
    outgoing.end();
  } catch (error) {
    console.error("AgentOS App request failed", error);
    if (!outgoing.headersSent) outgoing.writeHead(500);
    if (!outgoing.writableEnded) outgoing.end("Internal Server Error");
  }
}).listen(port, "0.0.0.0");
`;
}

export function staticRunnerSource(input: {
	root: string;
	release: string;
	port: number;
}): string {
	const root = normalizeAppPath(input.root === "." ? "index.html" : input.root);
	const staticRoot = input.root === "." ? "." : root;
	return `import http from "node:http";
import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { extname, join, normalize, resolve, sep } from "node:path";

const release = ${JSON.stringify(input.release)};
const port = ${input.port};
const root = resolve("/app", ${JSON.stringify(staticRoot)});
const types = {
  ".css": "text/css; charset=utf-8",
  ".gif": "image/gif",
  ".html": "text/html; charset=utf-8",
  ".ico": "image/x-icon",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
  ".webp": "image/webp",
};

http.createServer(async (request, response) => {
  try {
    const url = new URL(request.url ?? "/", "http://agentos-app");
    if (url.pathname === "/.agentos/ready") {
      response.setHeader("content-type", "application/json");
      response.end(JSON.stringify({ release }));
      return;
    }
    if (request.method !== "GET" && request.method !== "HEAD") {
      response.writeHead(405, { allow: "GET, HEAD" });
      response.end("Method Not Allowed");
      return;
    }
    const decoded = decodeURIComponent(url.pathname);
    const relative = normalize(decoded).replace(/^[/\\\\]+/, "");
    let path = resolve(root, relative || "index.html");
    if (path !== root && !path.startsWith(root + sep)) {
      response.writeHead(400);
      response.end("Bad Request");
      return;
    }
    let info;
    try {
      info = await stat(path);
      if (info.isDirectory()) {
        path = join(path, "index.html");
        info = await stat(path);
      }
    } catch {
      path = join(root, "index.html");
      info = await stat(path).catch(() => null);
    }
    if (!info?.isFile()) {
      response.writeHead(404);
      response.end("Not Found");
      return;
    }
    response.setHeader("content-type", types[extname(path).toLowerCase()] ?? "application/octet-stream");
    response.setHeader("content-length", String(info.size));
    if (request.method === "HEAD") {
      response.end();
      return;
    }
    createReadStream(path).on("error", (error) => {
      console.error("static response failed", error);
      if (!response.headersSent) response.writeHead(500);
      response.end();
    }).pipe(response);
  } catch (error) {
    console.error("static request failed", error);
    if (!response.headersSent) response.writeHead(500);
    response.end("Internal Server Error");
  }
}).listen(port, "0.0.0.0");
`;
}
