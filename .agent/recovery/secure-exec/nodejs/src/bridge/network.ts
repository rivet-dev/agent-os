// Network module polyfill for the sandbox
// Provides fetch, http, https, and dns module emulation that bridges to host

// Cap in-sandbox request/response buffering to prevent host memory exhaustion
const MAX_HTTP_BODY_BYTES = 50 * 1024 * 1024; // 50 MB

import type * as nodeHttp from "http";
import type * as nodeDns from "dns";
import type * as nodeDgram from "node:dgram";
import { exposeCustomGlobal } from "@secure-exec/core/internal/shared/global-exposure";
import type {
	FsFacadeBridge,
	NetworkDnsLookupRawBridgeRef,
	NetworkFetchRawBridgeRef,
	NetworkHttpRequestRawBridgeRef,
	NetworkHttpServerCloseRawBridgeRef,
	NetworkHttpServerListenRawBridgeRef,
	NetworkHttpServerRespondRawBridgeRef,
	NetworkHttpServerWaitRawBridgeRef,
	NetworkHttp2ServerCloseRawBridgeRef,
	NetworkHttp2ServerListenRawBridgeRef,
	NetworkHttp2ServerWaitRawBridgeRef,
	NetworkHttp2SessionCloseRawBridgeRef,
	NetworkHttp2SessionConnectRawBridgeRef,
	NetworkHttp2SessionRequestRawBridgeRef,
	NetworkHttp2SessionSettingsRawBridgeRef,
	NetworkHttp2SessionSetLocalWindowSizeRawBridgeRef,
	NetworkHttp2SessionGoawayRawBridgeRef,
	NetworkHttp2SessionDestroyRawBridgeRef,
	NetworkHttp2SessionWaitRawBridgeRef,
	NetworkHttp2ServerRespondRawBridgeRef,
	NetworkHttp2StreamEndRawBridgeRef,
	NetworkHttp2StreamCloseRawBridgeRef,
	NetworkHttp2StreamPauseRawBridgeRef,
	NetworkHttp2StreamResumeRawBridgeRef,
	NetworkHttp2StreamRespondWithFileRawBridgeRef,
	NetworkHttp2StreamPushStreamRawBridgeRef,
	NetworkHttp2StreamRespondRawBridgeRef,
	NetworkHttp2StreamWriteRawBridgeRef,
	RegisterHandleBridgeFn,
	UnregisterHandleBridgeFn,
	UpgradeSocketWriteRawBridgeRef,
	UpgradeSocketEndRawBridgeRef,
	UpgradeSocketDestroyRawBridgeRef,
	NetSocketConnectRawBridgeRef,
	NetSocketWaitConnectRawBridgeRef,
	NetSocketReadRawBridgeRef,
	NetSocketSetNoDelayRawBridgeRef,
	NetSocketSetKeepAliveRawBridgeRef,
	NetSocketWriteRawBridgeRef,
	NetSocketEndRawBridgeRef,
	NetSocketDestroyRawBridgeRef,
	NetSocketUpgradeTlsRawBridgeRef,
	NetSocketGetTlsClientHelloRawBridgeRef,
	NetSocketTlsQueryRawBridgeRef,
	NetServerListenRawBridgeRef,
	NetServerAcceptRawBridgeRef,
	NetServerCloseRawBridgeRef,
	TlsGetCiphersRawBridgeRef,
	DgramSocketCreateRawBridgeRef,
	DgramSocketBindRawBridgeRef,
	DgramSocketRecvRawBridgeRef,
	DgramSocketSendRawBridgeRef,
	DgramSocketCloseRawBridgeRef,
	DgramSocketAddressRawBridgeRef,
	DgramSocketSetBufferSizeRawBridgeRef,
	DgramSocketGetBufferSizeRawBridgeRef,
} from "../bridge-contract.js";

declare const _fdGetPath: {
  applySync(t: undefined, a: [number]): string | null;
};
declare const _fs: FsFacadeBridge;

// Declare host bridge References
declare const _networkFetchRaw: NetworkFetchRawBridgeRef;

declare const _networkDnsLookupRaw: NetworkDnsLookupRawBridgeRef;

declare const _networkHttpRequestRaw: NetworkHttpRequestRawBridgeRef;

declare const _networkHttpServerListenRaw:
  | NetworkHttpServerListenRawBridgeRef
  | undefined;

declare const _networkHttpServerCloseRaw:
  | NetworkHttpServerCloseRawBridgeRef
  | undefined;

declare const _networkHttpServerRespondRaw:
  | NetworkHttpServerRespondRawBridgeRef
  | undefined;

declare const _networkHttpServerWaitRaw:
  | NetworkHttpServerWaitRawBridgeRef
  | undefined;

declare const _networkHttp2ServerListenRaw:
  | NetworkHttp2ServerListenRawBridgeRef
  | undefined;

declare const _networkHttp2ServerCloseRaw:
  | NetworkHttp2ServerCloseRawBridgeRef
  | undefined;

declare const _networkHttp2ServerWaitRaw:
  | NetworkHttp2ServerWaitRawBridgeRef
  | undefined;

declare const _networkHttp2SessionConnectRaw:
  | NetworkHttp2SessionConnectRawBridgeRef
  | undefined;

declare const _networkHttp2SessionRequestRaw:
  | NetworkHttp2SessionRequestRawBridgeRef
  | undefined;

declare const _networkHttp2SessionSettingsRaw:
  | NetworkHttp2SessionSettingsRawBridgeRef
  | undefined;

declare const _networkHttp2SessionSetLocalWindowSizeRaw:
  | NetworkHttp2SessionSetLocalWindowSizeRawBridgeRef
  | undefined;

declare const _networkHttp2SessionGoawayRaw:
  | NetworkHttp2SessionGoawayRawBridgeRef
  | undefined;

declare const _networkHttp2SessionCloseRaw:
  | NetworkHttp2SessionCloseRawBridgeRef
  | undefined;

declare const _networkHttp2SessionDestroyRaw:
  | NetworkHttp2SessionDestroyRawBridgeRef
  | undefined;

declare const _networkHttp2SessionWaitRaw:
  | NetworkHttp2SessionWaitRawBridgeRef
  | undefined;

declare const _networkHttp2ServerRespondRaw:
  | NetworkHttp2ServerRespondRawBridgeRef
  | undefined;

declare const _networkHttp2StreamRespondRaw:
  | NetworkHttp2StreamRespondRawBridgeRef
  | undefined;

declare const _networkHttp2StreamPushStreamRaw:
  | NetworkHttp2StreamPushStreamRawBridgeRef
  | undefined;

declare const _networkHttp2StreamWriteRaw:
  | NetworkHttp2StreamWriteRawBridgeRef
  | undefined;

declare const _networkHttp2StreamEndRaw:
  | NetworkHttp2StreamEndRawBridgeRef
  | undefined;

declare const _networkHttp2StreamCloseRaw:
  | NetworkHttp2StreamCloseRawBridgeRef
  | undefined;

declare const _networkHttp2StreamPauseRaw:
  | NetworkHttp2StreamPauseRawBridgeRef
  | undefined;

declare const _networkHttp2StreamResumeRaw:
  | NetworkHttp2StreamResumeRawBridgeRef
  | undefined;

declare const _networkHttp2StreamRespondWithFileRaw:
  | NetworkHttp2StreamRespondWithFileRawBridgeRef
  | undefined;

declare const _netSocketConnectRaw:
  | NetSocketConnectRawBridgeRef
  | undefined;

declare const _netSocketWaitConnectRaw:
  | NetSocketWaitConnectRawBridgeRef
  | undefined;

declare const _netSocketReadRaw:
  | NetSocketReadRawBridgeRef
  | undefined;

declare const _netSocketSetNoDelayRaw:
  | NetSocketSetNoDelayRawBridgeRef
  | undefined;

declare const _netSocketSetKeepAliveRaw:
  | NetSocketSetKeepAliveRawBridgeRef
  | undefined;

declare const _netSocketWriteRaw:
  | NetSocketWriteRawBridgeRef
  | undefined;

declare const _netSocketEndRaw:
  | NetSocketEndRawBridgeRef
  | undefined;

declare const _netSocketDestroyRaw:
  | NetSocketDestroyRawBridgeRef
  | undefined;

declare const _netSocketUpgradeTlsRaw:
  | NetSocketUpgradeTlsRawBridgeRef
  | undefined;

declare const _netSocketGetTlsClientHelloRaw:
  | NetSocketGetTlsClientHelloRawBridgeRef
  | undefined;

declare const _netSocketTlsQueryRaw:
  | NetSocketTlsQueryRawBridgeRef
  | undefined;

declare const _netServerListenRaw:
  | NetServerListenRawBridgeRef
  | undefined;

declare const _netServerAcceptRaw:
  | NetServerAcceptRawBridgeRef
  | undefined;

declare const _netServerCloseRaw:
  | NetServerCloseRawBridgeRef
  | undefined;

declare const _dgramSocketCreateRaw:
  | DgramSocketCreateRawBridgeRef
  | undefined;

declare const _dgramSocketBindRaw:
  | DgramSocketBindRawBridgeRef
  | undefined;

declare const _dgramSocketRecvRaw:
  | DgramSocketRecvRawBridgeRef
  | undefined;

declare const _dgramSocketSendRaw:
  | DgramSocketSendRawBridgeRef
  | undefined;

declare const _dgramSocketCloseRaw:
  | DgramSocketCloseRawBridgeRef
  | undefined;

declare const _dgramSocketAddressRaw:
  | DgramSocketAddressRawBridgeRef
  | undefined;

declare const _dgramSocketSetBufferSizeRaw:
  | DgramSocketSetBufferSizeRawBridgeRef
  | undefined;

declare const _dgramSocketGetBufferSizeRaw:
  | DgramSocketGetBufferSizeRawBridgeRef
  | undefined;

declare const _tlsGetCiphersRaw:
  | TlsGetCiphersRawBridgeRef
  | undefined;

declare const _upgradeSocketWriteRaw:
  | UpgradeSocketWriteRawBridgeRef
  | undefined;

declare const _upgradeSocketEndRaw:
  | UpgradeSocketEndRawBridgeRef
  | undefined;

declare const _upgradeSocketDestroyRaw:
  | UpgradeSocketDestroyRawBridgeRef
  | undefined;

declare const _registerHandle:
  | RegisterHandleBridgeFn
  | undefined;

declare const _unregisterHandle:
  | UnregisterHandleBridgeFn
  | undefined;

// Types for fetch API
interface FetchOptions {
  method?: string;
  headers?: Headers | Record<string, string> | Array<[string, string]> | readonly string[];
  body?: string | null;
  mode?: string;
  credentials?: string;
  cache?: string;
  redirect?: string;
  referrer?: string;
  integrity?: string;
}

interface FetchResponse {
  ok: boolean;
  status: number;
  statusText: string;
  headers: Map<string, string>;
  url: string;
  redirected: boolean;
  type: string;
  body: ReadableStream<Uint8Array> | null;
  text(): Promise<string>;
  json(): Promise<unknown>;
  arrayBuffer(): Promise<ArrayBuffer>;
  blob(): Promise<never>;
  clone(): FetchResponse;
}

let _fetchHandleCounter = 0;

function encodeFetchBody(
  body: string,
  bodyEncoding: string | null,
): Uint8Array {
  if (bodyEncoding === "base64" && typeof Buffer !== "undefined") {
    return new Uint8Array(Buffer.from(body, "base64"));
  }
  if (typeof TextEncoder !== "undefined") {
    return new TextEncoder().encode(body);
  }
  const bytes = new Uint8Array(body.length);
  for (let index = 0; index < body.length; index += 1) {
    bytes[index] = body.charCodeAt(index) & 0xff;
  }
  return bytes;
}

function createFetchBodyStream(
  body: string,
  bodyEncoding: string | null,
): ReadableStream<Uint8Array> | null {
  const ReadableStreamCtor = globalThis.ReadableStream;
  if (typeof ReadableStreamCtor !== "function") {
    return null;
  }
  const bytes = encodeFetchBody(body, bodyEncoding);
  const handleId = typeof _registerHandle === "function"
    ? `fetch-body:${++_fetchHandleCounter}`
    : null;
  let released = false;
  let delivered = false;
  const release = () => {
    if (released || !handleId) {
      return;
    }
    released = true;
    _unregisterHandle?.(handleId);
  };
  if (handleId) {
    _registerHandle?.(handleId, "fetch response body");
  }
  return new ReadableStreamCtor<Uint8Array>({
    pull(controller) {
      if (delivered) {
        release();
        controller.close();
        return;
      }
      delivered = true;
      controller.enqueue(bytes);
      controller.close();
      release();
    },
    cancel() {
      release();
    },
  });
}

function serializeFetchHeaders(
  headers: FetchOptions["headers"] | undefined,
): Record<string, string> {
  if (!headers) {
    return {};
  }
  if (headers instanceof Headers) {
    return Object.fromEntries(headers.entries());
  }
  if (isFlatHeaderList(headers)) {
    const normalized: Record<string, string> = {};
    for (let index = 0; index < headers.length; index += 2) {
      const key = headers[index];
      const value = headers[index + 1];
      if (key !== undefined && value !== undefined) {
        normalized[key] = value;
      }
    }
    return normalized;
  }
  return Object.fromEntries(new Headers(headers).entries());
}

function createFetchHeaders(
  headers: FetchOptions["headers"] | Headers | undefined,
): Headers {
  return new Headers(serializeFetchHeaders(headers));
}

// Fetch polyfill
export async function fetch(input: string | URL | Request, options: FetchOptions = {}): Promise<FetchResponse> {
  if (typeof _networkFetchRaw === 'undefined') {
    console.error('fetch requires NetworkAdapter to be configured');
    throw new Error('fetch requires NetworkAdapter to be configured');
  }

  // Extract URL and options from Request object (used by axios fetch adapter)
  let resolvedUrl: string;
  if (input instanceof Request) {
    resolvedUrl = input.url;
    options = {
      method: input.method,
      headers: serializeFetchHeaders(input.headers),
      body: input.body,
      ...options,
    };
  } else {
    resolvedUrl = String(input);
  }

  const optionsJson = JSON.stringify({
    method: options.method || "GET",
    headers: serializeFetchHeaders(options.headers),
    body: options.body || null,
  });

  const handleId = typeof _registerHandle === "function"
    ? `fetch:${++_fetchHandleCounter}`
    : null;
  if (handleId) {
    _registerHandle?.(handleId, `fetch ${resolvedUrl}`);
  }

  try {
    const responseJson = await _networkFetchRaw.apply(undefined, [resolvedUrl, optionsJson], {
      result: { promise: true },
    });
    const response = JSON.parse(responseJson) as {
      ok: boolean;
      status: number;
      statusText: string;
      headers?: Record<string, string>;
      url?: string;
      redirected?: boolean;
      body?: string;
    };
    const bodyEncoding = response.headers?.["x-body-encoding"] ?? null;
    const responseBody = response.body ?? "";
    let bodyStream: ReadableStream<Uint8Array> | null | undefined;

    // Create Response-like object
    return {
      ok: response.ok,
      status: response.status,
      statusText: response.statusText,
      headers: new Map(Object.entries(response.headers || {})),
      url: response.url || resolvedUrl,
      redirected: response.redirected || false,
      type: "basic",
      get body() {
        if (bodyStream === undefined) {
          bodyStream = createFetchBodyStream(responseBody, bodyEncoding);
        }
        return bodyStream;
      },

      async text(): Promise<string> {
        if (bodyEncoding === "base64" && typeof Buffer !== "undefined") {
          return Buffer.from(responseBody, "base64").toString("utf8");
        }
        return responseBody;
      },
      async json(): Promise<unknown> {
        const textBody =
          bodyEncoding === "base64" && typeof Buffer !== "undefined"
            ? Buffer.from(responseBody, "base64").toString("utf8")
            : responseBody;
        return JSON.parse(textBody || "{}");
      },
      async arrayBuffer(): Promise<ArrayBuffer> {
        return Uint8Array.from(
          encodeFetchBody(responseBody, bodyEncoding),
        ).buffer;
      },
      async blob(): Promise<never> {
        throw new Error("Blob not supported in sandbox");
      },
      clone(): FetchResponse {
        return { ...this } as FetchResponse;
      },
    };
  } finally {
    if (handleId) {
      _unregisterHandle?.(handleId);
    }
  }
}

// Headers class
export class Headers {
  private _headers: Record<string, string> = {};

  constructor(init?: HeadersInit | Headers | Record<string, string> | [string, string][]) {
    if (init && init !== null) {
      if (init instanceof Headers) {
        this._headers = { ...init._headers };
      } else if (Array.isArray(init)) {
        init.forEach(([key, value]) => {
          this._headers[key.toLowerCase()] = value;
        });
      } else if (typeof init === "object") {
        Object.entries(init as Record<string, string>).forEach(([key, value]) => {
          this._headers[key.toLowerCase()] = value;
        });
      }
    }
  }

  get(name: string): string | null {
    return this._headers[name.toLowerCase()] || null;
  }

  set(name: string, value: string): void {
    this._headers[name.toLowerCase()] = value;
  }

  has(name: string): boolean {
    return name.toLowerCase() in this._headers;
  }

  delete(name: string): void {
    delete this._headers[name.toLowerCase()];
  }

  entries(): IterableIterator<[string, string]> {
    return Object.entries(this._headers)[Symbol.iterator]() as IterableIterator<[string, string]>;
  }

  [Symbol.iterator](): IterableIterator<[string, string]> {
    return this.entries();
  }

  keys(): IterableIterator<string> {
    return Object.keys(this._headers)[Symbol.iterator]();
  }

  values(): IterableIterator<string> {
    return Object.values(this._headers)[Symbol.iterator]();
  }

  append(name: string, value: string): void {
    const key = name.toLowerCase();
    if (key in this._headers) {
      this._headers[key] = this._headers[key] + ", " + value;
    } else {
      this._headers[key] = value;
    }
  }

  forEach(callback: (value: string, key: string, parent: Headers) => void): void {
    Object.entries(this._headers).forEach(([k, v]) => callback(v, k, this));
  }
}

// Request class
export class Request {
  url: string;
  method: string;
  headers: Headers;
  body: string | null;
  mode: string;
  credentials: string;
  cache: string;
  redirect: string;
  referrer: string;
  integrity: string;

  constructor(input: string | Request, init: FetchOptions = {}) {
    this.url = typeof input === "string" ? input : input.url;
    this.method = init.method || (typeof input !== "string" ? input.method : undefined) || "GET";
    this.headers = createFetchHeaders(
      init.headers || (typeof input !== "string" ? input.headers : undefined),
    );
    this.body = init.body || null;
    this.mode = init.mode || "cors";
    this.credentials = init.credentials || "same-origin";
    this.cache = init.cache || "default";
    this.redirect = init.redirect || "follow";
    this.referrer = init.referrer || "about:client";
    this.integrity = init.integrity || "";
  }

  clone(): Request {
    return new Request(this.url, this as unknown as FetchOptions);
  }
}

// Response class
export class Response {
  private _body: string | null;
  status: number;
  statusText: string;
  headers: Headers;
  ok: boolean;
  type: string;
  url: string;
  redirected: boolean;

  constructor(body?: string | null, init: { status?: number; statusText?: string; headers?: Record<string, string> } = {}) {
    this._body = body || null;
    this.status = init.status || 200;
    this.statusText = init.statusText || "OK";
    this.headers = new Headers(init.headers);
    this.ok = this.status >= 200 && this.status < 300;
    this.type = "default";
    this.url = "";
    this.redirected = false;
  }

  async text(): Promise<string> {
    return String(this._body || "");
  }

  async json(): Promise<unknown> {
    return JSON.parse(this._body || "{}");
  }

  get body(): { getReader(): { read(): Promise<{ done: boolean; value?: Uint8Array }> } } | null {
    const bodyStr = this._body;
    if (bodyStr === null) return null;
    return {
      getReader() {
        let consumed = false;
        return {
          async read() {
            if (consumed) return { done: true };
            consumed = true;
            const encoder = new TextEncoder();
            return { done: false, value: encoder.encode(bodyStr) };
          },
        };
      },
    };
  }

  clone(): Response {
    return new Response(this._body, { status: this.status, statusText: this.statusText });
  }

  static error(): Response {
    return new Response(null, { status: 0, statusText: "" });
  }

  static redirect(url: string, status = 302): Response {
    return new Response(null, { status, headers: { Location: url } });
  }
}

// DNS module types
type DnsCallback = (err: Error | null, address?: string, family?: number) => void;
type DnsResolveCallback = (err: Error | null, addresses?: string[]) => void;

interface DnsError extends Error {
  code?: string;
}

// DNS module polyfill
export const dns = {
  lookup(hostname: string, options: unknown, callback?: DnsCallback): void {
    let cb = callback;
    if (typeof options === "function") {
      cb = options as DnsCallback;
    }

    _networkDnsLookupRaw
      .apply(undefined, [hostname], { result: { promise: true } })
      .then((resultJson) => {
        const result = JSON.parse(resultJson) as { error?: string; code?: string; address?: string; family?: number };
        if (result.error) {
          const err: DnsError = new Error(result.error);
          err.code = result.code || "ENOTFOUND";
          cb?.(err);
        } else {
          cb?.(null, result.address, result.family);
        }
      })
      .catch((err) => {
        cb?.(err as Error);
      });
  },

  resolve(hostname: string, rrtype: string | DnsResolveCallback, callback?: DnsResolveCallback): void {
    let cb = callback;
    if (typeof rrtype === "function") {
      cb = rrtype;
    }

    // Simplified - just do lookup for A records
    dns.lookup(hostname, (err: Error | null, address?: string) => {
      if (err) {
        cb?.(err);
      } else {
        cb?.(null, address ? [address] : []);
      }
    });
  },

  resolve4(hostname: string, callback: DnsResolveCallback): void {
    dns.resolve(hostname, "A", callback);
  },

  resolve6(hostname: string, callback: DnsResolveCallback): void {
    dns.resolve(hostname, "AAAA", callback);
  },

  promises: {
    lookup(hostname: string, _options?: unknown): Promise<{ address: string; family: number }> {
      return new Promise((resolve, reject) => {
        dns.lookup(hostname, _options, (err, address, family) => {
          if (err) reject(err);
          else resolve({ address: address || "", family: family || 4 });
        });
      });
    },
    resolve(hostname: string, rrtype?: string): Promise<string[]> {
      return new Promise((resolve, reject) => {
        dns.resolve(hostname, rrtype || "A", (err, addresses) => {
          if (err) reject(err);
          else resolve(addresses || []);
        });
      });
    },
  },
};

// Event listener type
type EventListener = (...args: unknown[]) => void;

type RequestSocketLike = {
  destroyed: boolean;
  readable?: boolean;
  writable?: boolean;
  timeout?: number;
  _freeTimer?: ReturnType<typeof setTimeout> | null;
  on(event: string, listener: EventListener): unknown;
  once(event: string, listener: EventListener): unknown;
  off?(event: string, listener: EventListener): unknown;
  removeListener?(event: string, listener: EventListener): unknown;
  removeAllListeners?(event?: string): unknown;
  emit?(event: string, ...args: unknown[]): boolean;
  listeners?(event: string): EventListener[];
  listenerCount?(event: string): number;
  setTimeout?(timeout: number, callback?: () => void): unknown;
  setNoDelay?(noDelay?: boolean): unknown;
  setKeepAlive?(enable?: boolean, delay?: number): unknown;
  end?(...args: unknown[]): unknown;
  destroy(error?: Error): unknown;
};

function createConnResetError(message = "socket hang up"): Error & { code: string } {
  const error = new Error(message) as Error & { code: string };
  error.code = "ECONNRESET";
  return error;
}

function createAbortError(): Error & { code: string; name: string } {
  const error = new Error("The operation was aborted") as Error & {
    code: string;
    name: string;
  };
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  return error;
}

// Module-level globalAgent used by ClientRequest when no agent option is provided.
// Initialized lazily after Agent class is defined; set by createHttpModule().
let _moduleGlobalAgent: Agent | null = null;

/**
 * Polyfill of Node.js `http.IncomingMessage` (client-side response). Buffers
 * the response body eagerly and emits `data`/`end` events on listener
 * registration (flowing mode). Supports base64 binary decoding via
 * `x-body-encoding` header.
 */
export class IncomingMessage {
  headers: Record<string, string | string[]>;
  rawHeaders: string[];
  trailers: Record<string, string>;
  rawTrailers: string[];
  httpVersion: string;
  httpVersionMajor: number;
  httpVersionMinor: number;
  method: string | null;
  url: string;
  statusCode: number | undefined;
  statusMessage: string | undefined;
  private _body: string;
  private _isBinary: boolean;
  private _listeners: Record<string, EventListener[]>;
  complete: boolean;
  aborted: boolean;
  socket: FakeSocket | UpgradeSocket | DirectTunnelSocket | null;
  private _bodyConsumed: boolean;
  private _ended: boolean;
  private _flowing: boolean;
  readable: boolean;
  readableEnded: boolean;
  readableFlowing: boolean | null;
  destroyed: boolean;
  private _encoding?: string;
  private _closeEmitted: boolean;

  constructor(response?: {
    headers?: Record<string, string | string[]> | Array<[string, string]>;
    rawHeaders?: string[];
    url?: string;
    status?: number;
    statusText?: string;
    body?: string;
    trailers?: Record<string, string>;
    bodyEncoding?: "utf8" | "base64";
  }) {
    const normalizedHeaders: Record<string, string | string[]> = {};
    if (Array.isArray(response?.headers)) {
      response.headers.forEach(([key, value]) => {
        appendNormalizedHeader(normalizedHeaders, key.toLowerCase(), value);
      });
    } else if (response?.headers) {
      Object.entries(response.headers).forEach(([key, value]) => {
        normalizedHeaders[key] = Array.isArray(value) ? [...value] : value;
      });
    }
    this.rawHeaders = Array.isArray(response?.rawHeaders)
      ? [...response.rawHeaders]
      : [];
    if (this.rawHeaders.length > 0) {
      this.headers = {};
      for (let index = 0; index < this.rawHeaders.length; index += 2) {
        const key = this.rawHeaders[index];
        const value = this.rawHeaders[index + 1];
        if (key !== undefined && value !== undefined) {
          appendNormalizedHeader(this.headers, key.toLowerCase(), value);
        }
      }
    } else {
      this.headers = normalizedHeaders;
    }
    if (this.rawHeaders.length === 0 && this.headers && typeof this.headers === "object") {
      Object.entries(this.headers).forEach(([k, v]) => {
        if (Array.isArray(v)) {
          v.forEach((entry) => {
            this.rawHeaders.push(k, entry);
          });
          return;
        }
        this.rawHeaders.push(k, v);
      });
    }
    // Populate trailers if provided
    if (response?.trailers && typeof response.trailers === "object") {
      this.trailers = response.trailers;
      this.rawTrailers = [];
      Object.entries(response.trailers).forEach(([k, v]) => {
        this.rawTrailers.push(k, v);
      });
    } else {
      this.trailers = {};
      this.rawTrailers = [];
    }
    this.httpVersion = "1.1";
    this.httpVersionMajor = 1;
    this.httpVersionMinor = 1;
    this.method = null;
    this.url = response?.url || "";
    this.statusCode = response?.status;
    this.statusMessage = response?.statusText;
    // Decode base64 body if x-body-encoding header is set
    const bodyEncodingHeader = this.headers["x-body-encoding"];
    const bodyEncoding =
      response?.bodyEncoding ||
      (Array.isArray(bodyEncodingHeader) ? bodyEncodingHeader[0] : bodyEncodingHeader);
    if (bodyEncoding === 'base64' && response?.body && typeof Buffer !== 'undefined') {
      this._body = Buffer.from(response.body, 'base64').toString('binary');
      this._isBinary = true;
    } else {
      this._body = response?.body || "";
      this._isBinary = false;
    }
    this._listeners = {};
    this.complete = false;
    this.aborted = false;
    this.socket = null;
    this._bodyConsumed = false;
    this._ended = false;
    this._flowing = false;
    this.readable = true;
    this.readableEnded = false;
    this.readableFlowing = null;
    this.destroyed = false;
    this._closeEmitted = false;
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);

    // When 'data' listener is added, start flowing mode
    // Note: We check for non-empty body (this._body.length > 0) because we need to
    // emit 'end' even for empty responses, but only emit 'data' if there's actual data
    if (event === "data" && !this._bodyConsumed) {
      this._flowing = true;
      this.readableFlowing = true;
      // Emit data in next microtask
      Promise.resolve().then(() => {
        if (!this._bodyConsumed) {
          this._bodyConsumed = true;
          // Only emit data if there's actual content
          if (this._body && this._body.length > 0) {
            let buf: Buffer | string;
            if (typeof Buffer !== "undefined") {
              // For binary data, use 'binary' encoding to preserve bytes
              buf = this._isBinary ? Buffer.from(this._body, 'binary') : Buffer.from(this._body);
            } else {
              buf = this._body;
            }
            this.emit("data", buf);
          }
          // Always emit end after data (even if no data was emitted)
          Promise.resolve().then(() => {
            if (!this._ended) {
              this._ended = true;
              this.complete = true;
              this.readable = false;
              this.readableEnded = true;
              this.emit("end");
            }
          });
        }
      });
    }

    // If 'end' listener is added after data was already consumed, emit end
    if (event === "end" && this._bodyConsumed && !this._ended) {
      Promise.resolve().then(() => {
        if (!this._ended) {
          this._ended = true;
          this.complete = true;
          this.readable = false;
          this.readableEnded = true;
          listener();
        }
      });
    }

    return this;
  }

  once(event: string, listener: EventListener): this {
    const wrapper = (...args: unknown[]): void => {
      this.off(event, wrapper);
      listener(...args);
    };
    (wrapper as EventListener & { _originalListener?: EventListener })._originalListener = listener;
    return this.on(event, wrapper);
  }

  off(event: string, listener: EventListener): this {
    if (this._listeners[event]) {
      const idx = this._listeners[event].findIndex(
        (fn) => fn === listener || (fn as EventListener & { _originalListener?: EventListener })._originalListener === listener
      );
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  removeAllListeners(event?: string): this {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }

  emit(event: string, ...args: unknown[]): boolean {
    const handlers = this._listeners[event];
    if (handlers) {
      handlers.slice().forEach((fn) => fn(...args));
    }
    return handlers !== undefined && handlers.length > 0;
  }

  setEncoding(encoding: string): this {
    this._encoding = encoding;
    return this;
  }

  read(_size?: number): string | Buffer | null {
    if (this._bodyConsumed) return null;
    this._bodyConsumed = true;
    let buf: Buffer | string;
    if (typeof Buffer !== "undefined") {
      buf = this._isBinary ? Buffer.from(this._body, 'binary') : Buffer.from(this._body);
    } else {
      buf = this._body;
    }
    // Schedule end event
    Promise.resolve().then(() => {
      if (!this._ended) {
        this._ended = true;
        this.complete = true;
        this.readable = false;
        this.readableEnded = true;
        this.emit("end");
      }
    });
    return buf;
  }

  pipe<T extends NodeJS.WritableStream>(dest: T): T {
    let buf: Buffer | string;
    if (typeof Buffer !== "undefined") {
      buf = this._isBinary ? Buffer.from(this._body || "", 'binary') : Buffer.from(this._body || "");
    } else {
      buf = this._body || "";
    }
    if (typeof dest.write === "function" && (typeof buf === "string" ? buf.length : buf.length) > 0) {
      dest.write(buf as unknown as string);
    }
    if (typeof dest.end === "function") {
      Promise.resolve().then(() => dest.end());
    }
    this._bodyConsumed = true;
    this._ended = true;
    this.complete = true;
    this.readable = false;
    this.readableEnded = true;
    return dest;
  }

  pause(): this {
    this._flowing = false;
    this.readableFlowing = false;
    return this;
  }

  resume(): this {
    this._flowing = true;
    this.readableFlowing = true;
    if (!this._bodyConsumed) {
      Promise.resolve().then(() => {
        if (!this._bodyConsumed) {
          this._bodyConsumed = true;
          if (this._body) {
            let buf: Buffer | string;
            if (typeof Buffer !== "undefined") {
              buf = this._isBinary ? Buffer.from(this._body, 'binary') : Buffer.from(this._body);
            } else {
              buf = this._body;
            }
            this.emit("data", buf);
          }
          Promise.resolve().then(() => {
            if (!this._ended) {
              this._ended = true;
              this.complete = true;
              this.readable = false;
              this.readableEnded = true;
              this.emit("end");
            }
          });
        }
      });
    }
    return this;
  }

  unpipe(_dest?: NodeJS.WritableStream): this {
    return this;
  }

  destroy(err?: Error): this {
    this.destroyed = true;
    this.readable = false;
    if (err) this.emit("error", err);
    this._emitClose();
    return this;
  }

  _abort(err: Error = createConnResetError("aborted")): void {
    if (this.aborted) {
      return;
    }
    this.aborted = true;
    this.complete = false;
    this.destroyed = true;
    this.readable = false;
    this.readableEnded = true;
    this.emit("aborted");
    if (err) {
      this.emit("error", err);
    }
    this._emitClose();
  }

  private _emitClose(): void {
    if (this._closeEmitted) {
      return;
    }
    this._closeEmitted = true;
    this.emit("close");
  }

  [Symbol.asyncIterator](): AsyncIterator<string | Buffer> {
    const self = this;
    let dataEmitted = false;
    let ended = false;

    return {
      async next(): Promise<IteratorResult<string | Buffer>> {
        if (ended || self._ended) {
          return { done: true, value: undefined as unknown as string };
        }

        if (!dataEmitted && !self._bodyConsumed) {
          dataEmitted = true;
          self._bodyConsumed = true;
          let buf: Buffer | string;
          if (typeof Buffer !== "undefined") {
            buf = self._isBinary ? Buffer.from(self._body || "", 'binary') : Buffer.from(self._body || "");
          } else {
            buf = self._body || "";
          }
          return { done: false, value: buf };
        }

        ended = true;
        self._ended = true;
        self.complete = true;
        self.readable = false;
        self.readableEnded = true;
        return { done: true, value: undefined as unknown as string };
      },
      return(): Promise<IteratorResult<string | Buffer>> {
        ended = true;
        return Promise.resolve({ done: true, value: undefined as unknown as string });
      },
      throw(err: Error): Promise<IteratorResult<string | Buffer>> {
        ended = true;
        self.emit("error", err);
        return Promise.resolve({ done: true, value: undefined as unknown as string });
      },
    };
  }
}

/**
 * Polyfill of Node.js `http.ClientRequest`. Executes the request asynchronously
 * via the `_networkHttpRequestRaw` bridge and emits a `response` event with
 * an IncomingMessage. Supports Agent-based connection pooling, socket events,
 * HTTP upgrade (101), and trailer headers.
 */
export class ClientRequest {
  private _options: nodeHttp.RequestOptions;
  private _callback?: (res: IncomingMessage) => void;
  private _listeners: Record<string, EventListener[]> = {};
  private _headers: NormalizedHeaders = {};
  private _rawHeaderNames = new Map<string, string>();
  private _body = "";
  private _bodyBytes = 0;
  private _ended = false;
  private _agent: Agent | null;
  private _hostKey: string;
  private _socketEndListener: EventListener | null = null;
  private _socketCloseListener: EventListener | null = null;
  private _loopbackAbort?: () => void;
  private _response: IncomingMessage | null = null;
  private _closeEmitted = false;
  private _abortEmitted = false;
  private _signalAbortHandler?: () => void;
  private _signalPollTimer: ReturnType<typeof setTimeout> | null = null;
  private _skipExecute = false;
  private _destroyError: Error | undefined;
  private _errorEmitted = false;
  socket!: RequestSocketLike;
  finished = false;
  aborted = false;
  destroyed = false;
  path: string;
  method: string;
  reusedSocket = false;
  timeoutCb?: () => void;

  constructor(options: nodeHttp.RequestOptions, callback?: (res: IncomingMessage) => void) {
    const normalizedMethod = validateRequestMethod(options.method);
    this._options = {
      ...options,
      method: normalizedMethod,
      path: validateRequestPath(options.path),
    };
    this._callback = callback;
    this._validateTimeoutOption();
    this._setOutgoingHeaders(options.headers);
    if (!this._headers.host) {
      this._setHeaderValue("Host", buildHostHeader(this._options));
    }
    this.path = String(this._options.path || "/");
    this.method = String(this._options.method || "GET").toUpperCase();

    // Resolve agent: false = no agent, undefined = globalAgent, or explicit Agent
    const agentOpt = this._options.agent;
    if (agentOpt === false) {
      this._agent = null;
    } else if (agentOpt instanceof Agent) {
      this._agent = agentOpt;
    } else {
      this._agent = _moduleGlobalAgent;
    }
    this._hostKey = this._agent ? this._agent._getHostKey(this._options as { hostname?: string; host?: string; port?: string | number }) : "";
    this._bindAbortSignal();
    if (typeof this._options.timeout === "number") {
      this.setTimeout(this._options.timeout);
    }

    // Execute request asynchronously
    Promise.resolve().then(() => this._execute());
  }

  _assignSocket(socket: RequestSocketLike, reusedSocket: boolean): void {
    this.socket = socket;
    this.reusedSocket = reusedSocket;
    const trackedSocket = socket as RequestSocketLike & {
      _agentPermanentListenersInstalled?: boolean;
    };
    if (!trackedSocket._agentPermanentListenersInstalled) {
      trackedSocket._agentPermanentListenersInstalled = true;
      socket.on("error", () => {});
      socket.on("end", () => {});
    }
    this._socketEndListener = () => {};
    socket.on("end", this._socketEndListener);
    this._socketCloseListener = () => {
      this.destroyed = true;
      this._clearTimeout();
      this._emitClose();
    };
    socket.on("close", this._socketCloseListener);
    this._applyTimeoutToSocket(socket);
    this._emit("socket", socket);
    if (this.destroyed) {
      if (this._destroyError && !this._errorEmitted) {
        this._errorEmitted = true;
        queueMicrotask(() => {
          this._emit("error", this._destroyError);
        });
      }
      socket.destroy();
      return;
    }
    void this._dispatchWithSocket(socket);
  }

  _handleSocketError(err: Error): void {
    this._emit("error", err);
  }

  private _finalizeSocket(
    socket: RequestSocketLike,
    keepSocketAlive: boolean,
  ): void {
    if (this._socketEndListener) {
      socket.off?.("end", this._socketEndListener);
      socket.removeListener?.("end", this._socketEndListener);
      this._socketEndListener = null;
    }
    if (this._socketCloseListener) {
      socket.off?.("close", this._socketCloseListener);
      socket.removeListener?.("close", this._socketCloseListener);
      this._socketCloseListener = null;
    }
    if (this._agent) {
      this._agent._releaseSocket(this._hostKey, socket as FakeSocket, this._options, keepSocketAlive);
    } else if (!socket.destroyed) {
      socket.destroy();
    }
  }

  private async _dispatchWithSocket(socket: RequestSocketLike): Promise<void> {
    try {
      if (typeof _networkHttpRequestRaw === 'undefined') {
        console.error('http/https request requires NetworkAdapter to be configured');
        throw new Error('http/https request requires NetworkAdapter to be configured');
      }

      const url = this._buildUrl();
      const tls: Record<string, unknown> = {};
      if ((this._options as Record<string, unknown>).rejectUnauthorized !== undefined) {
        tls.rejectUnauthorized = (this._options as Record<string, unknown>).rejectUnauthorized;
      }
      const normalizedHeaders = normalizeRequestHeaders(this._options.headers);
      const requestMethod = String(this._options.method || "GET").toUpperCase();
      const responseJson = await _networkHttpRequestRaw.apply(undefined, [url, JSON.stringify({
        method: this._options.method || "GET",
        headers: normalizedHeaders,
        body: this._body || null,
        ...tls,
      })], {
        result: { promise: true },
      });
      const response = JSON.parse(responseJson) as {
        headers?: Record<string, string | string[]>;
        rawHeaders?: string[];
        url?: string;
        status?: number;
        statusText?: string;
        body?: string;
        bodyEncoding?: "utf8" | "base64";
        trailers?: Record<string, string>;
        informational?: SerializedInformationalResponse[];
        upgradeSocketId?: number;
      };

      this.finished = true;
      this._clearTimeout();

      // 101 Switching Protocols → fire 'upgrade' event
      if (response.status === 101) {
        const res = new IncomingMessage(response);
        // Use UpgradeSocket for bidirectional data relay when socketId is available
        let upgradeSocket: FakeSocket | UpgradeSocket | DirectTunnelSocket = socket as FakeSocket;
        if (response.upgradeSocketId != null) {
          upgradeSocket = new UpgradeSocket(response.upgradeSocketId, {
            host: this._options.hostname as string,
            port: Number(this._options.port) || 80,
          });
          upgradeSocketInstances.set(response.upgradeSocketId, upgradeSocket);
        }
        const head = typeof Buffer !== "undefined"
          ? (response.body ? Buffer.from(response.body, "base64") : Buffer.alloc(0))
          : new Uint8Array(0);
        res.socket = upgradeSocket;
        upgradeSocket.once("close", () => {
          this._emit("close");
        });
        if (this._listenerCount("upgrade") === 0) {
          process.nextTick(() => {
            this._finalizeSocket(socket, false);
          });
          upgradeSocket.destroy();
          return;
        }
        this._emit("upgrade", res, upgradeSocket, head);
        process.nextTick(() => {
          this._finalizeSocket(socket, false);
        });
        return;
      }

      if (requestMethod === "CONNECT" && response.upgradeSocketId != null) {
        const res = new IncomingMessage(response);
        const connectSocket = new UpgradeSocket(response.upgradeSocketId, {
          host: this._options.hostname as string,
          port: Number(this._options.port) || 80,
        });
        upgradeSocketInstances.set(response.upgradeSocketId, connectSocket);
        const head = typeof Buffer !== "undefined"
          ? (response.body ? Buffer.from(response.body, "base64") : Buffer.alloc(0))
          : new Uint8Array(0);
        res.socket = connectSocket;
        connectSocket.once("close", () => {
          this._emit("close");
        });
        this._emit("connect", res, connectSocket, head);
        process.nextTick(() => {
          this._finalizeSocket(socket, false);
        });
        return;
      }

      for (const informational of response.informational || []) {
        this._emit("information", new IncomingMessage({
          headers: Object.fromEntries(informational.headers || []),
          rawHeaders: informational.rawHeaders,
          status: informational.status,
          statusText: informational.statusText,
        }));
      }

      const res = new IncomingMessage(response);
      this._response = res;
      res.socket = socket as FakeSocket | UpgradeSocket | DirectTunnelSocket;
      res.once("end", () => {
        process.nextTick(() => {
          this._finalizeSocket(socket, this._agent?.keepAlive === true && !this.aborted);
        });
      });

      if (this._callback) {
        this._callback(res);
      }
      this._emit("response", res);
      if (!this._callback && this._listenerCount("response") === 0) {
        queueMicrotask(() => {
          res.resume();
        });
      }
    } catch (err) {
      this._clearTimeout();
      this._emit("error", err);
      this._finalizeSocket(socket, false);
    }
  }

  private _execute(): void {
    if (this._skipExecute) {
      return;
    }
    if (this._agent) {
      this._agent.addRequest(this, this._options);
      return;
    }
    const finish = (socket?: RequestSocketLike): void => {
      if (!socket) {
        this._handleSocketError(new Error("Failed to create socket"));
        this._emitClose();
        return;
      }
      this._assignSocket(socket, false);
    };

    const createConnection = this._options.createConnection;
    if (typeof createConnection === "function") {
      const maybeSocket = createConnection(this._options, (_err, socket) => {
        finish(socket as unknown as RequestSocketLike | undefined);
      });
      finish(maybeSocket as unknown as RequestSocketLike | undefined);
      return;
    }

    finish(new FakeSocket({
      host: (this._options.hostname || this._options.host || "localhost") as string,
      port: Number(this._options.port) || 80,
    }));
  }

  private _buildUrl(): string {
    const opts = this._options;
    const protocol = opts.protocol || (opts.port === 443 ? "https:" : "http:");
    const host = opts.hostname || opts.host || "localhost";
    const port = opts.port ? ":" + opts.port : "";
    const path = opts.path || "/";
    return protocol + "//" + host + port + path;
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  addListener(event: string, listener: EventListener): this {
    return this.on(event, listener);
  }

  once(event: string, listener: EventListener): this {
    const wrapper = (...args: unknown[]): void => {
      this.off(event, wrapper);
      listener(...args);
    };
    (
      wrapper as EventListener & {
        listener?: EventListener;
      }
    ).listener = listener;
    return this.on(event, wrapper);
  }

  off(event: string, listener: EventListener): this {
    if (this._listeners[event]) {
      const idx = this._listeners[event].findIndex(
        (registered) =>
          registered === listener ||
          (
            registered as EventListener & {
              listener?: EventListener;
            }
          ).listener === listener,
      );
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  getHeader(name: string): string | string[] | undefined {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE",
      );
    }
    return this._headers[name.toLowerCase()];
  }

  getHeaders(): Record<string, string | string[]> {
    const headers = Object.create(null) as Record<string, string | string[]>;
    for (const [key, value] of Object.entries(this._headers)) {
      headers[key] = Array.isArray(value) ? [...value] : value;
    }
    return headers;
  }

  getHeaderNames(): string[] {
    return Object.keys(this._headers);
  }

  getRawHeaderNames(): string[] {
    return Object.keys(this._headers).map((key) => this._rawHeaderNames.get(key) || key);
  }

  hasHeader(name: string): boolean {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE",
      );
    }
    return Object.prototype.hasOwnProperty.call(this._headers, name.toLowerCase());
  }

  removeHeader(name: string): void {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE",
      );
    }
    const lowerName = name.toLowerCase();
    delete this._headers[lowerName];
    this._rawHeaderNames.delete(lowerName);
    this._options.headers = { ...this._headers };
  }

  private _emit(event: string, ...args: unknown[]): void {
    if (this._listeners[event]) {
      this._listeners[event].forEach((fn) => fn(...args));
    }
  }

  private _listenerCount(event: string): number {
    return this._listeners[event]?.length || 0;
  }

  private _setOutgoingHeaders(headers: nodeHttp.OutgoingHttpHeaders | readonly string[] | undefined): void {
    this._headers = {};
    this._rawHeaderNames = new Map<string, string>();
    if (!headers) {
      this._options.headers = {};
      return;
    }

    if (Array.isArray(headers)) {
      for (let index = 0; index < headers.length; index += 2) {
        const key = headers[index];
        const value = headers[index + 1];
        if (key !== undefined && value !== undefined) {
          this._setHeaderValue(String(key), value);
        }
      }
      return;
    }

    Object.entries(headers).forEach(([key, value]) => {
      if (value !== undefined) {
        this._setHeaderValue(key, value);
      }
    });
  }

  private _setHeaderValue(
    name: string,
    value: string | number | readonly string[] | readonly number[],
  ): void {
    const actualName = validateHeaderName(name).toLowerCase();
    validateHeaderValue(actualName, value);
    this._headers[actualName] = Array.isArray(value)
      ? value.map((entry) => String(entry))
      : String(value);
    if (!this._rawHeaderNames.has(actualName)) {
      this._rawHeaderNames.set(actualName, name);
    }
    this._options.headers = { ...this._headers };
  }

  write(data: string): boolean {
    const addedBytes = typeof Buffer !== "undefined" ? Buffer.byteLength(data) : data.length;
    if (this._bodyBytes + addedBytes > MAX_HTTP_BODY_BYTES) {
      throw new Error("ERR_HTTP_BODY_TOO_LARGE: request body exceeds " + MAX_HTTP_BODY_BYTES + " byte limit");
    }
    this._body += data;
    this._bodyBytes += addedBytes;
    return true;
  }

  end(data?: string): this {
    if (data) this.write(data);
    this._ended = true;
    return this;
  }

  abort(): void {
    if (this.aborted) {
      return;
    }
    this.aborted = true;
    if (!this._abortEmitted) {
      this._abortEmitted = true;
      queueMicrotask(() => {
        this._emit("abort");
      });
    }
    this._loopbackAbort?.();
    this.destroy();
  }

  destroy(err?: Error): this {
    if (this.destroyed) {
      return this;
    }
    this.destroyed = true;
    this._clearTimeout();
    this._unbindAbortSignal();
    this._loopbackAbort?.();
    this._loopbackAbort = undefined;
    if (!this.socket && err && (err as { code?: string }).code === "ABORT_ERR") {
      this._skipExecute = true;
    }

    const responseStarted = this._response != null;
    const destroyError =
      err ??
      (!this.aborted && !responseStarted ? createConnResetError() : undefined);
    this._destroyError = destroyError;

    if (this._response && !this._response.complete && !this._response.aborted) {
      this._response._abort(destroyError ?? createConnResetError("aborted"));
    }

    if (this.socket && !this.socket.destroyed) {
      if (destroyError && !this._errorEmitted) {
        this._errorEmitted = true;
        queueMicrotask(() => {
          this._emit("error", destroyError);
        });
      }
      this.socket.destroy(destroyError);
    } else {
      if (destroyError) {
        this._errorEmitted = true;
        queueMicrotask(() => {
          this._emit("error", destroyError);
        });
      }
      queueMicrotask(() => {
        this._emitClose();
      });
    }
    return this;
  }

  setTimeout(timeout: number, callback?: () => void): this {
    if (callback) {
      this.once("timeout", callback);
    }
    this.timeoutCb = () => {
      this._emit("timeout");
    };
    this._clearTimeout();
    if (timeout === 0) {
      return this;
    }
    if (!Number.isFinite(timeout) || timeout < 0) {
      throw new TypeError(`The "timeout" argument must be of type number. Received ${String(timeout)}`);
    }
    this._options.timeout = timeout;
    if (this.socket) {
      this._applyTimeoutToSocket(this.socket);
    }
    return this;
  }

  setNoDelay(): this {
    return this;
  }

  setSocketKeepAlive(): this {
    return this;
  }

  flushHeaders(): void {
    // no-op
  }

  private _emitClose(): void {
    if (this._closeEmitted) {
      return;
    }
    this._closeEmitted = true;
    this._emit("close");
  }

  private _applyTimeoutToSocket(socket: RequestSocketLike): void {
    const timeout = this._options.timeout;
    if (typeof timeout !== "number" || timeout === 0) {
      return;
    }
    if (!this.timeoutCb) {
      this.timeoutCb = () => {
        this._emit("timeout");
      };
    }
    socket.off?.("timeout", this.timeoutCb);
    socket.removeListener?.("timeout", this.timeoutCb);
    socket.setTimeout?.(timeout, this.timeoutCb);
  }

  private _validateTimeoutOption(): void {
    const timeout = this._options.timeout;
    if (timeout === undefined) {
      return;
    }
    if (typeof timeout !== "number") {
      const received = timeout === null
        ? "null"
        : typeof timeout === "string"
          ? `type string ('${timeout}')`
          : `type ${typeof timeout} (${JSON.stringify(timeout)})`;
      const error = new TypeError(`The "timeout" argument must be of type number. Received ${received}`) as TypeError & {
        code?: string;
      };
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
  }

  private _bindAbortSignal(): void {
    const signal = this._options.signal;
    if (!signal) {
      return;
    }
    this._signalAbortHandler = () => {
      this.destroy(createAbortError());
    };
    if (signal.aborted) {
      this.destroyed = true;
      this._skipExecute = true;
      queueMicrotask(() => {
        this._emit("error", createAbortError());
        this._emitClose();
      });
      return;
    }
    if (typeof signal.addEventListener === "function") {
      signal.addEventListener("abort", this._signalAbortHandler, { once: true });
      return;
    }

    const signalWithOnAbort = signal as AbortSignal & {
      onabort?: ((this: AbortSignal, event: Event) => void) | null;
      __secureExecPrevOnAbort__?: ((this: AbortSignal, event: Event) => void) | null;
    };
    signalWithOnAbort.__secureExecPrevOnAbort__ = signalWithOnAbort.onabort ?? null;
    signalWithOnAbort.onabort = ((event: Event) => {
      signalWithOnAbort.__secureExecPrevOnAbort__?.call(signal, event);
      this._signalAbortHandler?.();
    }) as (this: AbortSignal, event: Event) => void;
    this._startAbortSignalPoll(signal);
  }

  private _unbindAbortSignal(): void {
    const signal = this._options.signal;
    if (!signal || !this._signalAbortHandler) {
      return;
    }
    if (this._signalPollTimer) {
      clearTimeout(this._signalPollTimer);
      this._signalPollTimer = null;
    }
    if (typeof signal.removeEventListener === "function") {
      signal.removeEventListener("abort", this._signalAbortHandler);
      this._signalAbortHandler = undefined;
      return;
    }

    const signalWithOnAbort = signal as AbortSignal & {
      onabort?: ((this: AbortSignal, event: Event) => void) | null;
      __secureExecPrevOnAbort__?: ((this: AbortSignal, event: Event) => void) | null;
    };
    if (signalWithOnAbort.onabort === this._signalAbortHandler) {
      signalWithOnAbort.onabort = signalWithOnAbort.__secureExecPrevOnAbort__ ?? null;
    } else if (signalWithOnAbort.__secureExecPrevOnAbort__ !== undefined) {
      signalWithOnAbort.onabort = signalWithOnAbort.__secureExecPrevOnAbort__ ?? null;
    }
    delete signalWithOnAbort.__secureExecPrevOnAbort__;
    this._signalAbortHandler = undefined;
  }

  private _startAbortSignalPoll(signal: AbortSignal): void {
    const poll = (): void => {
      if (this.destroyed) {
        this._signalPollTimer = null;
        return;
      }
      if (signal.aborted) {
        this._signalPollTimer = null;
        this._signalAbortHandler?.();
        return;
      }
      this._signalPollTimer = setTimeout(poll, 5);
    };

    if (!this._signalPollTimer) {
      this._signalPollTimer = setTimeout(poll, 5);
    }
  }

  private _clearTimeout(): void {
    if (this.socket && this.timeoutCb) {
      this.socket.off?.("timeout", this.timeoutCb);
      this.socket.removeListener?.("timeout", this.timeoutCb);
    }
    if (this.socket?.setTimeout) {
      this.socket.setTimeout(0);
    }
  }
}

// Minimal socket-like object emitted by ClientRequest 'socket' event
class FakeSocket {
  remoteAddress: string;
  remotePort: number;
  localAddress = "127.0.0.1";
  localPort = 0;
  connecting = false;
  destroyed = false;
  writable = true;
  readable = true;
  timeout = 0;
  private _listeners: Record<string, EventListener[]> = {};
  private _closed = false;
  private _closeScheduled = false;
  private _timeoutTimer: ReturnType<typeof setTimeout> | null = null;
  _freeTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(options?: { host?: string; port?: number }) {
    this.remoteAddress = options?.host || "127.0.0.1";
    this.remotePort = options?.port || 80;
  }

  setTimeout(ms: number, cb?: () => void): this {
    this.timeout = ms;
    if (cb) {
      this.on("timeout", cb);
    }
    if (this._timeoutTimer) {
      clearTimeout(this._timeoutTimer);
      this._timeoutTimer = null;
    }
    if (ms > 0) {
      this._timeoutTimer = setTimeout(() => {
        this.emit("timeout");
      }, ms);
    }
    return this;
  }
  setNoDelay(_noDelay?: boolean): this { return this; }
  setKeepAlive(_enable?: boolean, _delay?: number): this { return this; }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: EventListener): this {
    const wrapper = (...args: unknown[]): void => {
      this.off(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }

  off(event: string, listener: EventListener): this {
    if (this._listeners[event]) {
      const idx = this._listeners[event].indexOf(listener);
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  removeAllListeners(event?: string): this {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }

  emit(event: string, ...args: unknown[]): boolean {
    const handlers = this._listeners[event];
    if (handlers) handlers.slice().forEach((fn) => fn(...args));
    return handlers !== undefined && handlers.length > 0;
  }

  listenerCount(event: string): number {
    return this._listeners[event]?.length || 0;
  }

  listeners(event: string): EventListener[] {
    return [...(this._listeners[event] || [])];
  }

  write(_data: unknown): boolean { return true; }
  end(): this {
    if (this.destroyed || this._closed) return this;
    this.writable = false;
    queueMicrotask(() => {
      if (this.destroyed || this._closed) return;
      this.readable = false;
      this.emit("end");
      this.destroy();
    });
    return this;
  }

  destroy(): this {
    if (this.destroyed || this._closed) return this;
    this.destroyed = true;
    this._closed = true;
    this.writable = false;
    this.readable = false;
    if (this._timeoutTimer) {
      clearTimeout(this._timeoutTimer);
      this._timeoutTimer = null;
    }
    if (!this._closeScheduled) {
      this._closeScheduled = true;
      queueMicrotask(() => {
        this._closeScheduled = false;
        this.emit("close");
      });
    }
    return this;
  }
}

class DirectTunnelSocket {
  remoteAddress: string;
  remotePort: number;
  localAddress = "127.0.0.1";
  localPort = 0;
  connecting = false;
  destroyed = false;
  writable = true;
  readable = true;
  readyState = "open";
  bytesWritten = 0;
  private _listeners: Record<string, EventListener[]> = {};
  private _encoding?: BufferEncoding;
  private _peer: DirectTunnelSocket | null = null;
  _readableState = { endEmitted: false };
  _writableState = { finished: false, errorEmitted: false };

  constructor(options?: { host?: string; port?: number }) {
    this.remoteAddress = options?.host || "127.0.0.1";
    this.remotePort = options?.port || 80;
  }

  _attachPeer(peer: DirectTunnelSocket): void {
    this._peer = peer;
  }

  setTimeout(_ms: number, _cb?: () => void): this { return this; }
  setNoDelay(_noDelay?: boolean): this { return this; }
  setKeepAlive(_enable?: boolean, _delay?: number): this { return this; }
  setEncoding(encoding: BufferEncoding): this {
    this._encoding = encoding;
    return this;
  }
  ref(): this { return this; }
  unref(): this { return this; }
  cork(): void {}
  uncork(): void {}
  pause(): this { return this; }
  resume(): this { return this; }
  address(): { address: string; family: string; port: number } {
    return { address: this.localAddress, family: "IPv4", port: this.localPort };
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: EventListener): this {
    const wrapper = (...args: unknown[]): void => {
      this.off(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }

  off(event: string, listener: EventListener): this {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  removeAllListeners(event?: string): this {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }

  emit(event: string, ...args: unknown[]): boolean {
    const listeners = this._listeners[event];
    if (!listeners || listeners.length === 0) return false;
    listeners.slice().forEach((listener) => listener.call(this, ...args));
    return true;
  }

  listenerCount(event: string): number {
    return this._listeners[event]?.length || 0;
  }

  write(data: unknown, encodingOrCb?: string | (() => void), cb?: (() => void)): boolean {
    if (this.destroyed || !this._peer) return false;
    const callback = typeof encodingOrCb === "function" ? encodingOrCb : cb;
    const buffer = normalizeSocketChunk(data);
    this.bytesWritten += buffer.length;
    queueMicrotask(() => {
      this._peer?._pushData(buffer);
    });
    callback?.();
    return true;
  }

  end(data?: unknown): this {
    if (data !== undefined) {
      this.write(data);
    }
    this.writable = false;
    this._writableState.finished = true;
    queueMicrotask(() => {
      this._peer?._pushEnd();
    });
    this.emit("finish");
    return this;
  }

  destroy(err?: Error): this {
    if (this.destroyed) return this;
    this.destroyed = true;
    this.readable = false;
    this.writable = false;
    this._readableState.endEmitted = true;
    this._writableState.finished = true;
    if (err) {
      this.emit("error", err);
    }
    queueMicrotask(() => {
      this._peer?._pushEnd();
    });
    this.emit("close", false);
    return this;
  }

  _pushData(buffer: Buffer): void {
    if (!this.readable || this.destroyed) {
      return;
    }
    this.emit("data", this._encoding ? buffer.toString(this._encoding) : buffer);
  }

  _pushEnd(): void {
    if (this.destroyed) {
      return;
    }
    this.readable = false;
    this.writable = false;
    this._readableState.endEmitted = true;
    this._writableState.finished = true;
    this.emit("end");
    this.emit("close", false);
  }
}

function normalizeSocketChunk(data: unknown): Buffer {
  if (typeof Buffer !== "undefined" && Buffer.isBuffer(data)) {
    return data;
  }
  if (data instanceof Uint8Array) {
    return Buffer.from(data);
  }
  return Buffer.from(String(data));
}

type QueuedAgentRequest = {
  request: ClientRequest;
  options: nodeHttp.RequestOptions;
};

// HTTP Agent with connection pooling via maxSockets/maxTotalSockets
class Agent {
  static defaultMaxSockets = Infinity;

  maxSockets: number;
  maxTotalSockets: number;
  maxFreeSockets: number;
  keepAlive: boolean;
  keepAliveMsecs: number;
  timeout: number;
  requests: Record<string, QueuedAgentRequest[]>;
  sockets: Record<string, FakeSocket[]>;
  freeSockets: Record<string, FakeSocket[]>;
  totalSocketCount: number;
  private _listeners: Record<string, EventListener[]> = {};

  constructor(options?: {
    keepAlive?: boolean;
    keepAliveMsecs?: number;
    maxSockets?: number;
    maxTotalSockets?: number;
    maxFreeSockets?: number;
    timeout?: number;
  }) {
    this._validateSocketCountOption("maxSockets", options?.maxSockets);
    this._validateSocketCountOption("maxFreeSockets", options?.maxFreeSockets);
    this._validateSocketCountOption("maxTotalSockets", options?.maxTotalSockets);
    this.keepAlive = options?.keepAlive ?? false;
    this.keepAliveMsecs = options?.keepAliveMsecs ?? 1000;
    this.maxSockets = options?.maxSockets ?? Agent.defaultMaxSockets;
    this.maxTotalSockets = options?.maxTotalSockets ?? Infinity;
    this.maxFreeSockets = options?.maxFreeSockets ?? 256;
    this.timeout = options?.timeout ?? -1;
    this.requests = {};
    this.sockets = {};
    this.freeSockets = {};
    this.totalSocketCount = 0;
  }

  private _validateSocketCountOption(
    name: "maxSockets" | "maxFreeSockets" | "maxTotalSockets",
    value: number | undefined,
  ): void {
    if (value === undefined) return;
    if (typeof value !== "number") {
      const received =
        typeof value === "string"
          ? `type string ('${value}')`
          : `type ${typeof value} (${JSON.stringify(value)})`;
      const err = new TypeError(
        `The "${name}" argument must be of type number. Received ${received}`,
      ) as TypeError & { code?: string };
      err.code = "ERR_INVALID_ARG_TYPE";
      throw err;
    }
    if (Number.isNaN(value) || value <= 0) {
      const err = new RangeError(
        `The value of "${name}" is out of range. It must be > 0. Received ${String(value)}`,
      ) as RangeError & { code?: string };
      err.code = "ERR_OUT_OF_RANGE";
      throw err;
    }
  }

  getName(options?: {
    hostname?: string | null;
    host?: string | null;
    port?: string | number | null;
    localAddress?: string | null;
    family?: string | number | null;
    socketPath?: string | null;
  }): string {
    const host = options?.hostname || options?.host || "localhost";
    const port = options?.port ?? "";
    const localAddress = options?.localAddress ?? "";
    let suffix = "";
    if (options?.socketPath) {
      suffix = `:${options.socketPath}`;
    } else if (options?.family === 4 || options?.family === 6) {
      suffix = `:${options.family}`;
    }
    return `${host}:${port}:${localAddress}${suffix}`;
  }

  _getHostKey(options: {
    hostname?: string | null;
    host?: string | null;
    port?: string | number | null;
    localAddress?: string | null;
    family?: string | number | null;
    socketPath?: string | null;
  }): string {
    return this.getName(options);
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: EventListener): this {
    const wrapper = (...args: unknown[]): void => {
      this.off(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }

  off(event: string, listener: EventListener): this {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  emit(event: string, ...args: unknown[]): boolean {
    const listeners = this._listeners[event];
    if (!listeners || listeners.length === 0) return false;
    listeners.slice().forEach((listener) => listener(...args));
    return true;
  }

  createConnection(
    options: nodeHttp.RequestOptions & {
      keepAlive?: boolean;
      keepAliveInitialDelay?: number;
    },
    cb?: (err: Error | null, socket?: FakeSocket) => void,
  ): FakeSocket {
    if (typeof options.createConnection === "function") {
      return options.createConnection(
        options,
        (cb ?? (() => undefined)) as (err: Error | null, socket: unknown) => void,
      ) as unknown as FakeSocket;
    }
    const socket = new FakeSocket({
      host: String(options.hostname || options.host || "localhost"),
      port: Number(options.port) || 80,
    });
    if (cb) {
      Promise.resolve().then(() => cb(null, socket));
    }
    return socket;
  }

  addRequest(request: ClientRequest, options: nodeHttp.RequestOptions): void {
    const name = this.getName(options);
    const freeSocket = this._takeFreeSocket(name);
    if (freeSocket) {
      this._activateSocket(name, freeSocket);
      request._assignSocket(freeSocket, true);
      return;
    }

    if (this._canCreateSocket(name)) {
      this._createSocketForRequest(name, request, options);
      return;
    }

    if (!this.requests[name]) {
      this.requests[name] = [];
    }
    this.requests[name].push({ request, options });
  }

  _releaseSocket(
    name: string,
    socket: FakeSocket,
    options: nodeHttp.RequestOptions,
    keepSocketAlive: boolean,
  ): void {
    const removedActive = this._removeSocket(this.sockets, name, socket);
    if (keepSocketAlive && !socket.destroyed) {
      const freeList = this.freeSockets[name] ?? (this.freeSockets[name] = []);
      if (freeList.length < this.maxFreeSockets) {
        if (socket._freeTimer) {
          clearTimeout(socket._freeTimer);
          socket._freeTimer = null;
        }
        freeList.push(socket);
        if (this.timeout > 0) {
          socket._freeTimer = setTimeout(() => {
            socket._freeTimer = null;
            socket.destroy();
          }, this.timeout);
        }
        socket.emit("free");
        this.emit("free", socket, options);
      } else {
        if (removedActive) {
          this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
        }
        socket.destroy();
      }
    } else if (!socket.destroyed) {
      if (removedActive) {
        this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
      }
      socket.destroy();
    }
    Promise.resolve().then(() => this._processPendingRequests());
  }

  _removeSocketCompletely(name: string, socket: FakeSocket): void {
    if (socket._freeTimer) {
      clearTimeout(socket._freeTimer);
      socket._freeTimer = null;
    }
    const removed =
      this._removeSocket(this.sockets, name, socket) ||
      this._removeSocket(this.freeSockets, name, socket);
    if (removed) {
      this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
      Promise.resolve().then(() => this._processPendingRequests());
    }
  }

  private _canCreateSocket(name: string): boolean {
    const activeCount = this.sockets[name]?.length ?? 0;
    if (activeCount >= this.maxSockets) {
      return false;
    }
    if (this.totalSocketCount < this.maxTotalSockets) {
      return true;
    }
    this._evictFreeSocket(name);
    return this.totalSocketCount < this.maxTotalSockets;
  }

  private _takeFreeSocket(name: string): FakeSocket | null {
    const freeList = this.freeSockets[name];
    while (freeList && freeList.length > 0) {
      const socket = freeList.shift()!;
      if (!socket.destroyed) {
        if (socket._freeTimer) {
          clearTimeout(socket._freeTimer);
          socket._freeTimer = null;
        }
        if (freeList.length === 0) delete this.freeSockets[name];
        return socket;
      }
      this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
    }
    if (freeList && freeList.length === 0) {
      delete this.freeSockets[name];
    }
    return null;
  }

  private _activateSocket(name: string, socket: FakeSocket): void {
    const activeList = this.sockets[name] ?? (this.sockets[name] = []);
    activeList.push(socket);
  }

  private _createSocketForRequest(
    name: string,
    request: ClientRequest,
    options: nodeHttp.RequestOptions,
  ): void {
    let settled = false;
    const finish = (err: Error | null, socket?: FakeSocket): void => {
      if (settled) return;
      settled = true;
      if (err || !socket) {
        request._handleSocketError(err ?? new Error("Failed to create socket"));
        this._processPendingRequests();
        return;
      }
      if (request.destroyed) {
        this.totalSocketCount += 1;
        this._activateSocket(name, socket);
        socket.once("close", () => {
          this._removeSocketCompletely(name, socket);
        });
        request._assignSocket(socket, false);
        return;
      }
      this.totalSocketCount += 1;
      this._activateSocket(name, socket);
      socket.once("close", () => {
        this._removeSocketCompletely(name, socket);
      });
      request._assignSocket(socket, false);
    };

    const connectionOptions = {
      ...options,
      keepAlive: this.keepAlive,
      keepAliveInitialDelay: this.keepAliveMsecs,
    };

    try {
      const maybeSocket = this.createConnection(connectionOptions, (err, socket) => {
        finish(err, socket);
      });
      if (maybeSocket) {
        finish(null, maybeSocket);
      }
    } catch (err) {
      finish(err instanceof Error ? err : new Error(String(err)));
    }
  }

  private _processPendingRequests(): void {
    for (const name of Object.keys(this.requests)) {
      const queue = this.requests[name];
      while (queue && queue.length > 0) {
        const freeSocket = this._takeFreeSocket(name);
        if (freeSocket) {
          const entry = queue.shift()!;
          if (entry.request.destroyed) {
            this._activateSocket(name, freeSocket);
            this._releaseSocket(name, freeSocket, entry.options, true);
            continue;
          }
          this._activateSocket(name, freeSocket);
          entry.request._assignSocket(freeSocket, true);
          continue;
        }
        if (!this._canCreateSocket(name)) {
          break;
        }
        const entry = queue.shift()!;
        if (entry.request.destroyed) {
          continue;
        }
        this._createSocketForRequest(name, entry.request, entry.options);
      }
      if (!queue || queue.length === 0) {
        delete this.requests[name];
      }
    }
  }

  private _removeSocket(
    sockets: Record<string, FakeSocket[]>,
    name: string,
    socket: FakeSocket,
  ): boolean {
    const list = sockets[name];
    if (!list) return false;
    const index = list.indexOf(socket);
    if (index === -1) return false;
    list.splice(index, 1);
    if (list.length === 0) delete sockets[name];
    return true;
  }

  private _evictFreeSocket(preferredName: string): void {
    const keys = Object.keys(this.freeSockets);
    const orderedKeys = keys.includes(preferredName)
      ? [...keys.filter((key) => key !== preferredName), preferredName]
      : keys;
    for (const key of orderedKeys) {
      const socket = this.freeSockets[key]?.[0];
      if (!socket) continue;
      socket.destroy();
      return;
    }
  }

  destroy(): void {
    for (const socket of Object.values(this.sockets).flat()) {
      socket.destroy();
    }
    for (const socket of Object.values(this.freeSockets).flat()) {
      socket.destroy();
    }
    this.requests = {};
    this.sockets = {};
    this.freeSockets = {};
    this.totalSocketCount = 0;
  }
}

interface ServerAddress {
  address: string;
  family: string;
  port: number;
}

interface SerializedServerListenResult {
  address: ServerAddress | null;
}

interface SerializedServerRequest {
  method: string;
  url: string;
  headers: Record<string, string | string[]>;
  rawHeaders: string[];
  bodyBase64?: string;
}

interface SerializedServerResponse {
  status: number;
  headers?: Array<[string, string]>;
  rawHeaders?: string[];
  informational?: SerializedInformationalResponse[];
  body?: string;
  bodyEncoding?: "utf8" | "base64";
  trailers?: Array<[string, string]>;
  rawTrailers?: string[];
  connectionEnded?: boolean;
  connectionReset?: boolean;
  upgradeSocketId?: number;
}

interface SerializedInformationalResponse {
  status: number;
  statusText?: string;
  headers?: Array<[string, string]>;
  rawHeaders?: string[];
}

function debugBridgeNetwork(...args: unknown[]): void {
  if (process.env.SECURE_EXEC_DEBUG_HTTP_BRIDGE === "1") {
    console.error("[secure-exec bridge network]", ...args);
  }
}

let nextServerId = 1;
// Server instances indexed by serverId — used by request/upgrade dispatch
const serverInstances = new Map<number, Server>();

const HTTP_METHODS = [
  "ACL",
  "BIND",
  "CHECKOUT",
  "CONNECT",
  "COPY",
  "DELETE",
  "GET",
  "HEAD",
  "LINK",
  "LOCK",
  "M-SEARCH",
  "MERGE",
  "MKACTIVITY",
  "MKCALENDAR",
  "MKCOL",
  "MOVE",
  "NOTIFY",
  "OPTIONS",
  "PATCH",
  "POST",
  "PROPFIND",
  "PROPPATCH",
  "PURGE",
  "PUT",
  "QUERY",
  "REBIND",
  "REPORT",
  "SEARCH",
  "SOURCE",
  "SUBSCRIBE",
  "TRACE",
  "UNBIND",
  "UNLINK",
  "UNLOCK",
  "UNSUBSCRIBE",
];

type NormalizedHeaderValue = string | string[];
type NormalizedHeaders = Record<string, NormalizedHeaderValue>;
type StoredHeaderValue = string | number | Array<string | number>;
type LoopbackRequestParseResult =
  | {
    kind: "incomplete";
  }
  | {
    kind: "bad-request";
    closeConnection: boolean;
  }
  | {
    kind: "request";
    bytesConsumed: number;
    request: SerializedServerRequest;
    closeConnection: boolean;
    upgradeHead?: Buffer;
  };

const INVALID_REQUEST_PATH_REGEXP = /[^\u0021-\u00ff]/;
const HTTP_TOKEN_EXTRA_CHARS = new Set(["!", "#", "$", "%", "&", "'", "*", "+", "-", ".", "^", "_", "`", "|", "~"]);

function createTypeErrorWithCode(message: string, code: string): TypeError & { code: string } {
  const error = new TypeError(message) as TypeError & { code: string };
  error.code = code;
  return error;
}

function createErrorWithCode(message: string, code: string): Error & { code: string } {
  const error = new Error(message) as Error & { code: string };
  error.code = code;
  return error;
}

function formatReceivedType(value: unknown): string {
  if (value === null) {
    return "null";
  }
  if (Array.isArray(value)) {
    return "an instance of Array";
  }
  const valueType = typeof value;
  if (valueType === "function") {
    const name =
      typeof (value as { name?: unknown }).name === "string" &&
      (value as { name?: string }).name!.length > 0
        ? (value as { name?: string }).name!
        : "anonymous";
    return `function ${name}`;
  }
  if (valueType === "object") {
    const ctorName =
      value &&
      typeof value === "object" &&
      typeof (value as { constructor?: { name?: string } }).constructor?.name === "string"
        ? (value as { constructor?: { name?: string } }).constructor!.name!
        : "Object";
    return `an instance of ${ctorName}`;
  }
  if (valueType === "string") {
    return `type string ('${String(value)}')`;
  }
  if (valueType === "symbol") {
    return `type symbol (${String(value)})`;
  }
  return `type ${valueType} (${String(value)})`;
}

function createInvalidArgTypeError(argumentName: string, expectedType: string, value: unknown): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The "${argumentName}" property must be of type ${expectedType}. Received ${formatReceivedType(value)}`,
    "ERR_INVALID_ARG_TYPE",
  );
}

function checkIsHttpToken(value: string): boolean {
  if (value.length === 0) {
    return false;
  }
  for (let index = 0; index < value.length; index += 1) {
    const char = value[index];
    const code = value.charCodeAt(index);
    const isAlphaNum =
      (code >= 48 && code <= 57) ||
      (code >= 65 && code <= 90) ||
      (code >= 97 && code <= 122);
    if (!isAlphaNum && !HTTP_TOKEN_EXTRA_CHARS.has(char)) {
      return false;
    }
  }
  return true;
}

function checkInvalidHeaderChar(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code === 9) {
      continue;
    }
    if (code < 32 || code === 127 || code > 255) {
      return true;
    }
  }
  return false;
}

function validateHeaderName(name: unknown, label = "Header name"): string {
  const actualName = String(name);
  if (!checkIsHttpToken(actualName)) {
    throw createTypeErrorWithCode(
      `${label} must be a valid HTTP token [${JSON.stringify(actualName)}]`,
      "ERR_INVALID_HTTP_TOKEN",
    );
  }
  return actualName;
}

function validateHeaderValue(name: string, value: unknown): void {
  if (value === undefined) {
    throw createTypeErrorWithCode(
      `Invalid value "undefined" for header "${name}"`,
      "ERR_HTTP_INVALID_HEADER_VALUE",
    );
  }
  if (Array.isArray(value)) {
    for (const entry of value) {
      validateHeaderValue(name, entry);
    }
    return;
  }
  if (checkInvalidHeaderChar(String(value))) {
    throw createTypeErrorWithCode(
      `Invalid character in header content [${JSON.stringify(name)}]`,
      "ERR_INVALID_CHAR",
    );
  }
}

function serializeHeaderValue(value: StoredHeaderValue): string | string[] {
  if (Array.isArray(value)) {
    return value.map((entry) => String(entry));
  }
  return String(value);
}

function joinHeaderValue(value: NormalizedHeaderValue): string {
  return Array.isArray(value) ? value.join(", ") : value;
}

function cloneStoredHeaderValue(value: StoredHeaderValue): StoredHeaderValue {
  return Array.isArray(value) ? [...value] : value;
}

function appendNormalizedHeader(
  target: NormalizedHeaders,
  key: string,
  value: string,
): void {
  if (key === "set-cookie") {
    const existing = target[key];
    if (existing === undefined) {
      target[key] = [value];
    } else if (Array.isArray(existing)) {
      existing.push(value);
    } else {
      target[key] = [existing, value];
    }
    return;
  }

  const existing = target[key];
  target[key] =
    existing === undefined
      ? value
      : `${joinHeaderValue(existing)}, ${value}`;
}

function flattenRawHeaders(headers: NormalizedHeaders): string[] {
  const rawHeaders: string[] = [];
  for (const [key, value] of Object.entries(headers)) {
    if (Array.isArray(value)) {
      value.forEach((entry) => {
        rawHeaders.push(key, entry);
      });
      continue;
    }
    rawHeaders.push(key, value);
  }
  return rawHeaders;
}

function validateRequestMethod(method: unknown): string | undefined {
  if (method == null || method === "") {
    return undefined;
  }
  if (typeof method !== "string") {
    throw createInvalidArgTypeError("options.method", "string", method);
  }
  return validateHeaderName(method, "Method");
}

function validateRequestPath(path: unknown): string {
  const resolvedPath = path == null || path === "" ? "/" : String(path);
  if (INVALID_REQUEST_PATH_REGEXP.test(resolvedPath)) {
    throw createTypeErrorWithCode(
      "Request path contains unescaped characters",
      "ERR_UNESCAPED_CHARACTERS",
    );
  }
  return resolvedPath;
}

function buildHostHeader(options: nodeHttp.RequestOptions): string {
  const host = String(options.hostname || options.host || "localhost");
  const defaultPort =
    options.protocol === "https:" || Number(options.port) === 443
      ? 443
      : 80;
  const port = options.port != null ? Number(options.port) : defaultPort;
  return port === defaultPort ? host : `${host}:${port}`;
}

function isFlatHeaderList(
  headers: Record<string, string> | Array<[string, string]> | readonly string[],
): headers is readonly string[] {
  return Array.isArray(headers) && (headers.length === 0 || typeof headers[0] === "string");
}

function normalizeRequestHeaders(
  headers: nodeHttp.OutgoingHttpHeaders | readonly string[] | undefined,
) : NormalizedHeaders {
  if (!headers) return {};
  if (Array.isArray(headers)) {
    const normalized: NormalizedHeaders = {};
    for (let i = 0; i < headers.length; i += 2) {
      const key = headers[i];
      const value = headers[i + 1];
      if (key !== undefined && value !== undefined) {
        const normalizedKey = validateHeaderName(key).toLowerCase();
        validateHeaderValue(normalizedKey, value);
        appendNormalizedHeader(normalized, normalizedKey, String(value));
      }
    }
    return normalized;
  }

  const normalized: NormalizedHeaders = {};
  Object.entries(headers).forEach(([key, value]) => {
    if (value === undefined) return;
    const normalizedKey = validateHeaderName(key).toLowerCase();
    validateHeaderValue(normalizedKey, value);
    if (Array.isArray(value)) {
      value.forEach((entry) => appendNormalizedHeader(normalized, normalizedKey, String(entry)));
      return;
    }
    appendNormalizedHeader(normalized, normalizedKey, String(value));
  });
  return normalized;
}

function hasUpgradeRequestHeaders(headers: NormalizedHeaders): boolean {
  const connectionHeader = joinHeaderValue(headers.connection || "").toLowerCase();
  return connectionHeader.includes("upgrade") && Boolean(headers.upgrade);
}

function hasResponseBody(statusCode: number, method?: string): boolean {
  if (method === "HEAD") {
    return false;
  }
  if ((statusCode >= 100 && statusCode < 200) || statusCode === 204 || statusCode === 304) {
    return false;
  }
  return true;
}

function splitTransferEncodingTokens(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim().toLowerCase())
    .filter((entry) => entry.length > 0);
}

function parseContentLengthHeader(value: NormalizedHeaderValue | undefined): number | null {
  if (value === undefined) {
    return 0;
  }

  const entries = Array.isArray(value) ? value : [value];
  let parsed: number | null = null;
  for (const entry of entries) {
    if (!/^\d+$/.test(entry)) {
      return null;
    }
    const nextValue = Number(entry);
    if (!Number.isSafeInteger(nextValue) || nextValue < 0) {
      return null;
    }
    if (parsed !== null && parsed !== nextValue) {
      return null;
    }
    parsed = nextValue;
  }
  return parsed ?? 0;
}

function parseChunkedBody(
  bodyBuffer: Buffer,
): { complete: false } | { complete: true; bytesConsumed: number; body: Buffer } | null {
  let offset = 0;
  const chunks: Buffer[] = [];

  while (true) {
    const lineEnd = bodyBuffer.indexOf("\r\n", offset);
    if (lineEnd === -1) {
      return { complete: false };
    }

    const sizeLine = bodyBuffer.subarray(offset, lineEnd).toString("latin1");
    if (sizeLine.length === 0 || /[\r\n]/.test(sizeLine)) {
      return null;
    }
    const [sizePart, extensionPart] = sizeLine.split(";", 2);
    if (!/^[0-9A-Fa-f]+$/.test(sizePart)) {
      return null;
    }
    if (extensionPart !== undefined && /[\r\n]/.test(extensionPart)) {
      return null;
    }

    const chunkSize = Number.parseInt(sizePart, 16);
    if (!Number.isSafeInteger(chunkSize) || chunkSize < 0) {
      return null;
    }

    const chunkStart = lineEnd + 2;
    const chunkEnd = chunkStart + chunkSize;
    const chunkTerminatorEnd = chunkEnd + 2;
    if (chunkTerminatorEnd > bodyBuffer.length) {
      return { complete: false };
    }
    if (
      bodyBuffer[chunkEnd] !== 13 ||
      bodyBuffer[chunkEnd + 1] !== 10
    ) {
      return null;
    }

    if (chunkSize > 0) {
      chunks.push(bodyBuffer.subarray(chunkStart, chunkEnd));
      offset = chunkTerminatorEnd;
      continue;
    }

    const trailersEnd = bodyBuffer.indexOf("\r\n\r\n", chunkStart);
    if (trailersEnd === -1) {
      return { complete: false };
    }

    const trailerBlock = bodyBuffer.subarray(chunkStart, trailersEnd).toString("latin1");
    if (trailerBlock.length > 0) {
      for (const trailerLine of trailerBlock.split("\r\n")) {
        if (trailerLine.length === 0) {
          continue;
        }
        if (trailerLine.startsWith(" ") || trailerLine.startsWith("\t")) {
          return null;
        }
        if (trailerLine.indexOf(":") === -1) {
          return null;
        }
      }
    }

    return {
      complete: true,
      bytesConsumed: trailersEnd + 4,
      body: chunks.length > 0 ? Buffer.concat(chunks) : Buffer.alloc(0),
    };
  }
}

function parseLoopbackRequestBuffer(
  buffer: Buffer,
  server: Server,
): LoopbackRequestParseResult {
  let requestStart = 0;
  while (
    requestStart + 1 < buffer.length &&
    buffer[requestStart] === 13 &&
    buffer[requestStart + 1] === 10
  ) {
    requestStart += 2;
  }

  const headerEnd = buffer.indexOf("\r\n\r\n", requestStart);
  if (headerEnd === -1) {
    return { kind: "incomplete" };
  }

  const headerBlock = buffer.subarray(requestStart, headerEnd).toString("latin1");
  const [requestLine, ...headerLines] = headerBlock.split("\r\n");
  const requestMatch = /^([A-Z]+)\s+(\S+)\s+HTTP\/(1)\.(0|1)$/.exec(requestLine);
  if (!requestMatch) {
    return {
      kind: "bad-request",
      closeConnection: true,
    };
  }

  const headers: NormalizedHeaders = {};
  const rawHeaders: string[] = [];
  let previousHeaderName: string | null = null;

  try {
    for (const headerLine of headerLines) {
      if (headerLine.length === 0) {
        continue;
      }
      if (headerLine.startsWith(" ") || headerLine.startsWith("\t")) {
        return {
          kind: "bad-request",
          closeConnection: true,
        };
      }

      const separatorIndex = headerLine.indexOf(":");
      if (separatorIndex === -1) {
        return {
          kind: "bad-request",
          closeConnection: true,
        };
      }

      const rawName = headerLine.slice(0, separatorIndex).trim();
      const rawValue = headerLine.slice(separatorIndex + 1).trim();
      const normalizedName = validateHeaderName(rawName).toLowerCase();
      validateHeaderValue(normalizedName, rawValue);
      appendNormalizedHeader(headers, normalizedName, rawValue);
      rawHeaders.push(rawName, rawValue);
      previousHeaderName = normalizedName;
    }
  } catch {
    return {
      kind: "bad-request",
      closeConnection: true,
    };
  }

  const requestMethod = requestMatch[1];
  const requestUrl = requestMatch[2];
  const httpMinorVersion = Number(requestMatch[4]);
  const requestCloseHeader = joinHeaderValue(headers.connection || "").toLowerCase();
  let closeConnection = httpMinorVersion === 0
    ? !requestCloseHeader.includes("keep-alive")
    : requestCloseHeader.includes("close");

  if (hasUpgradeRequestHeaders(headers) && server.listenerCount("upgrade") > 0) {
    return {
      kind: "request",
      bytesConsumed: buffer.length,
      closeConnection: false,
      request: {
        method: requestMethod,
        url: requestUrl,
        headers,
        rawHeaders,
        bodyBase64: headerEnd + 4 < buffer.length
          ? buffer.subarray(headerEnd + 4).toString("base64")
          : undefined,
      },
      upgradeHead: headerEnd + 4 < buffer.length
        ? buffer.subarray(headerEnd + 4)
        : Buffer.alloc(0),
    };
  }

  const transferEncoding = headers["transfer-encoding"];
  const contentLength = headers["content-length"];
  let requestBody: Buffer<ArrayBufferLike> = Buffer.alloc(0);
  let bytesConsumed = headerEnd + 4;

  if (transferEncoding !== undefined) {
    const tokens = splitTransferEncodingTokens(joinHeaderValue(transferEncoding));
    const chunkedCount = tokens.filter((entry) => entry === "chunked").length;
    const hasChunked = chunkedCount > 0;
    const chunkedIsFinal = hasChunked && tokens[tokens.length - 1] === "chunked";
    if (!hasChunked || chunkedCount !== 1 || !chunkedIsFinal || contentLength !== undefined) {
      return {
        kind: "bad-request",
        closeConnection: true,
      };
    }

    const parsedChunked = parseChunkedBody(buffer.subarray(headerEnd + 4));
    if (parsedChunked === null) {
      return {
        kind: "bad-request",
        closeConnection: true,
      };
    }
    if (!parsedChunked.complete) {
      return { kind: "incomplete" };
    }

    requestBody = parsedChunked.body;
    bytesConsumed = headerEnd + 4 + parsedChunked.bytesConsumed;
  } else if (contentLength !== undefined) {
    const parsedContentLength = parseContentLengthHeader(contentLength);
    if (parsedContentLength === null) {
      return {
        kind: "bad-request",
        closeConnection: true,
      };
    }
    const bodyEnd = headerEnd + 4 + parsedContentLength;
    if (bodyEnd > buffer.length) {
      return { kind: "incomplete" };
    }
    requestBody = buffer.subarray(headerEnd + 4, bodyEnd);
    bytesConsumed = bodyEnd;
  }

  return {
    kind: "request",
    bytesConsumed,
    closeConnection,
    request: {
      method: requestMethod,
      url: requestUrl,
      headers,
      rawHeaders,
      bodyBase64: requestBody.length > 0 ? requestBody.toString("base64") : undefined,
    },
  };
}

function serializeRawHeaderPairs(
  rawHeaders: string[] | undefined,
  fallbackHeaders: Array<[string, string]> | undefined,
): {
  headers: NormalizedHeaders;
  rawNameMap: Map<string, string>;
  order: string[];
} {
  const headers: NormalizedHeaders = {};
  const rawNameMap = new Map<string, string>();
  const order: string[] = [];

  if (Array.isArray(rawHeaders) && rawHeaders.length > 0) {
    for (let index = 0; index < rawHeaders.length; index += 2) {
      const rawName = rawHeaders[index];
      const value = rawHeaders[index + 1];
      if (rawName === undefined || value === undefined) {
        continue;
      }
      const normalizedName = rawName.toLowerCase();
      appendNormalizedHeader(headers, normalizedName, value);
      if (!rawNameMap.has(normalizedName)) {
        rawNameMap.set(normalizedName, rawName);
        order.push(normalizedName);
      }
    }
    return { headers, rawNameMap, order };
  }

  if (Array.isArray(fallbackHeaders)) {
    for (const [name, value] of fallbackHeaders) {
      const normalizedName = name.toLowerCase();
      appendNormalizedHeader(headers, normalizedName, value);
      if (!rawNameMap.has(normalizedName)) {
        rawNameMap.set(normalizedName, name);
        order.push(normalizedName);
      }
    }
  }

  return { headers, rawNameMap, order };
}

function finalizeRawHeaderPairs(
  headers: NormalizedHeaders,
  rawNameMap: Map<string, string>,
  order: string[],
): Array<[string, string]> {
  const entries: Array<[string, string]> = [];
  const seen = new Set<string>();
  for (const key of order) {
    const value = headers[key];
    if (value === undefined) {
      continue;
    }
    const rawName = rawNameMap.get(key) || key;
    const serialized = Array.isArray(value)
      ? (key === "set-cookie" ? value : [value.join(", ")])
      : [value];
    for (const entry of serialized) {
      entries.push([rawName, entry]);
    }
    seen.add(key);
  }

  for (const [key, value] of Object.entries(headers)) {
    if (seen.has(key)) {
      continue;
    }
    const rawName = rawNameMap.get(key) || key;
    const serialized = Array.isArray(value)
      ? (key === "set-cookie" ? value : [value.join(", ")])
      : [value];
    for (const entry of serialized) {
      entries.push([rawName, entry]);
    }
  }

  return entries;
}

function createBadRequestResponseBuffer(): Buffer {
  return Buffer.from("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n", "latin1");
}

function serializeLoopbackResponse(
  response: SerializedServerResponse,
  request: SerializedServerRequest,
  requestWantsClose: boolean,
): { payload: Buffer; closeConnection: boolean } {
  const statusCode = response.status || 200;
  const statusText = HTTP_STATUS_TEXT[statusCode] || "OK";
  const {
    headers,
    rawNameMap,
    order,
  } = serializeRawHeaderPairs(response.rawHeaders, response.headers);
  const trailerInfo = serializeRawHeaderPairs(response.rawTrailers, response.trailers);

  const bodyBuffer =
    response.body == null
      ? Buffer.alloc(0)
      : response.bodyEncoding === "base64"
        ? Buffer.from(response.body, "base64")
        : Buffer.from(response.body, "utf8");
  const bodyAllowed = hasResponseBody(statusCode, request.method);
  const transferEncodingTokens = headers["transfer-encoding"]
    ? splitTransferEncodingTokens(joinHeaderValue(headers["transfer-encoding"]))
    : [];
  const isChunked = transferEncodingTokens.includes("chunked");
  const hasExplicitContentLength = headers["content-length"] !== undefined;
  let closeConnection =
    requestWantsClose ||
    response.connectionEnded === true ||
    response.connectionReset === true;

  if (!bodyAllowed) {
    if (isChunked) {
      closeConnection = true;
    }
    delete headers["content-length"];
  } else if (!isChunked && !hasExplicitContentLength) {
    headers["content-length"] = String(bodyBuffer.length);
    rawNameMap.set("content-length", "Content-Length");
    order.push("content-length");
  }

  if (closeConnection) {
    headers.connection = "close";
    if (!rawNameMap.has("connection")) {
      rawNameMap.set("connection", "Connection");
      order.push("connection");
    }
  } else if (headers.connection === undefined && request.headers.connection !== undefined) {
    headers.connection = "keep-alive";
    rawNameMap.set("connection", "Connection");
    order.push("connection");
  }

  const serializedChunks: Buffer[] = [];
  for (const informational of response.informational ?? []) {
    const infoHeaders = finalizeRawHeaderPairs(
      serializeRawHeaderPairs(informational.rawHeaders, informational.headers).headers,
      serializeRawHeaderPairs(informational.rawHeaders, informational.headers).rawNameMap,
      serializeRawHeaderPairs(informational.rawHeaders, informational.headers).order,
    );
    const headerLines = infoHeaders.map(([name, value]) => `${name}: ${value}\r\n`).join("");
    serializedChunks.push(
      Buffer.from(
        `HTTP/1.1 ${informational.status} ${informational.statusText || HTTP_STATUS_TEXT[informational.status] || ""}\r\n${headerLines}\r\n`,
        "latin1",
      ),
    );
  }

  const finalHeaders = finalizeRawHeaderPairs(headers, rawNameMap, order);
  const headerLines = finalHeaders.map(([name, value]) => `${name}: ${value}\r\n`).join("");
  serializedChunks.push(
    Buffer.from(`HTTP/1.1 ${statusCode} ${statusText}\r\n${headerLines}\r\n`, "latin1"),
  );

  if (bodyAllowed) {
    if (isChunked) {
      if (bodyBuffer.length > 0) {
        serializedChunks.push(Buffer.from(bodyBuffer.length.toString(16) + "\r\n", "latin1"));
        serializedChunks.push(bodyBuffer);
        serializedChunks.push(Buffer.from("\r\n", "latin1"));
      }
      serializedChunks.push(Buffer.from("0\r\n", "latin1"));
      if (Object.keys(trailerInfo.headers).length > 0) {
        const trailerPairs = finalizeRawHeaderPairs(
          trailerInfo.headers,
          trailerInfo.rawNameMap,
          trailerInfo.order,
        );
        for (const [name, value] of trailerPairs) {
          serializedChunks.push(Buffer.from(`${name}: ${value}\r\n`, "latin1"));
        }
      }
      serializedChunks.push(Buffer.from("\r\n", "latin1"));
    } else if (bodyBuffer.length > 0) {
      serializedChunks.push(bodyBuffer);
    }
  }

  return {
    payload: serializedChunks.length === 1 ? serializedChunks[0] : Buffer.concat(serializedChunks),
    closeConnection,
  };
}

const HTTP_STATUS_TEXT: Record<number, string> = {
  100: "Continue",
  101: "Switching Protocols",
  102: "Processing",
  103: "Early Hints",
  200: "OK",
  201: "Created",
  204: "No Content",
  301: "Moved Permanently",
  302: "Found",
  304: "Not Modified",
  400: "Bad Request",
  401: "Unauthorized",
  403: "Forbidden",
  404: "Not Found",
  500: "Internal Server Error",
};

function isLoopbackRequestHost(hostname: string): boolean {
  const bare = hostname.startsWith("[") && hostname.endsWith("]")
    ? hostname.slice(1, -1)
    : hostname;
  return bare === "localhost" || bare === "127.0.0.1" || bare === "::1";
}

function findLoopbackServerForRequest(
  options: nodeHttp.RequestOptions,
): Server | null {
  if (String(options.method || "GET").toUpperCase() === "CONNECT") {
    return null;
  }
  return findLoopbackServerByPort(options, true);
}

function findLoopbackServerByPort(
  options: nodeHttp.RequestOptions,
  skipUpgradeHeaders = false,
): Server | null {
  const hostname = String(options.hostname || options.host || "localhost");
  if (!isLoopbackRequestHost(hostname)) {
    return null;
  }

  const normalizedHeaders = normalizeRequestHeaders(options.headers);
  if (skipUpgradeHeaders && hasUpgradeRequestHeaders(normalizedHeaders)) {
    return null;
  }

  const port = Number(options.port) || 80;
  for (const server of serverInstances.values()) {
    const address = server.address();
    if (!address) continue;
    if (address.port === port) {
      return server;
    }
  }

  return null;
}

function findLoopbackHttp2CompatibilityServer(
  options: nodeHttp.RequestOptions,
): Http2Server | null {
  const hostname = String(options.hostname || options.host || "localhost");
  if (!isLoopbackRequestHost(hostname)) {
    return null;
  }

  const port = Number(options.port) || 443;
  for (const server of http2Servers.values()) {
    const address = server.address();
    if (!address || typeof address !== "object") {
      continue;
    }
    if (
      address.port === port &&
      server.encrypted &&
      server.allowHTTP1 &&
      server.listenerCount("request") > 0
    ) {
      return server;
    }
  }

  return null;
}

class ServerIncomingMessage {
  headers: Record<string, string | string[]>;
  rawHeaders: string[];
  method: string;
  url: string;
  socket: Record<string, unknown>;
  connection: Record<string, unknown>;
  rawBody?: Buffer;
  destroyed = false;
  errored?: Error;
  readable = true;
  httpVersion = "1.1";
  httpVersionMajor = 1;
  httpVersionMinor = 1;
  complete = true;
  aborted = false;
  // Readable stream state stub for frameworks that inspect internal state
  _readableState = { flowing: null, length: 0, ended: false, objectMode: false };
  private _listeners: Record<string, EventListener[]> = {};

  constructor(request: SerializedServerRequest) {
    this.headers = request.headers || {};
    this.rawHeaders = request.rawHeaders || [];
    if (!Array.isArray(this.rawHeaders) || this.rawHeaders.length % 2 !== 0) {
      this.rawHeaders = [];
    }
    this.method = request.method || "GET";
    this.url = request.url || "/";
    const fakeSocket: Record<string, unknown> = {
      encrypted: false,
      remoteAddress: "127.0.0.1",
      remotePort: 0,
      writable: true,
      on() { return fakeSocket; },
      once() { return fakeSocket; },
      removeListener() { return fakeSocket; },
      destroy() {},
      end() {},
    };
    this.socket = fakeSocket;
    this.connection = fakeSocket;
    const rawHost = this.headers.host;
    if (typeof rawHost === "string" && rawHost.includes(",")) {
      this.headers.host = rawHost.split(",")[0].trim();
    }
    if (!this.headers.host) {
      this.headers.host = "127.0.0.1";
    }
    if (this.rawHeaders.length === 0) {
      Object.entries(this.headers).forEach(([key, value]) => {
        if (Array.isArray(value)) {
          value.forEach((entry) => {
            this.rawHeaders.push(key, entry);
          });
          return;
        }
        this.rawHeaders.push(key, value);
      });
    }
    if (request.bodyBase64 && typeof Buffer !== "undefined") {
      this.rawBody = Buffer.from(request.bodyBase64, "base64");
    }
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: EventListener): this {
    const wrapped = (...args: unknown[]): void => {
      this.off(event, wrapped);
      listener(...args);
    };
    return this.on(event, wrapped);
  }

  off(event: string, listener: EventListener): this {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  emit(event: string, ...args: unknown[]): boolean {
    const listeners = this._listeners[event];
    if (!listeners || listeners.length === 0) return false;
    listeners.slice().forEach((fn) => fn(...args));
    return true;
  }

  // Readable stream stubs for framework compatibility
  unpipe(): this { return this; }
  pause(): this { return this; }
  resume(): this { return this; }
  read(): null { return null; }
  pipe(dest: unknown): unknown { return dest; }
  isPaused(): boolean { return false; }
  setEncoding(): this { return this; }

  destroy(err?: Error): this {
    this.destroyed = true;
    this.errored = err;
    if (err) {
      this.emit("error", err);
    }
    this.emit("close");
    return this;
  }

  _abort(): void {
    if (this.aborted) {
      return;
    }
    this.aborted = true;
    const error = createConnResetError("aborted");
    this.emit("aborted");
    this.emit("error", error);
    this.emit("close");
  }
}

/**
 * Sandbox-side response writer for HTTP server requests. Collects headers and
 * body chunks, then serializes to JSON for transfer back to the host.
 */
class ServerResponseBridge {
  statusCode = 200;
  statusMessage = "OK";
  headersSent = false;
  writable = true;
  writableFinished = false;
  outputSize = 0;
  private _headers = new Map<string, StoredHeaderValue>();
  private _trailers = new Map<string, StoredHeaderValue>();
  private _chunks: Uint8Array[] = [];
  private _chunksBytes = 0;
  private _listeners: Record<string, EventListener[]> = {};
  private _closedPromise: Promise<void>;
  private _resolveClosed: (() => void) | null = null;
  private _connectionEnded = false;
  private _connectionReset = false;
  private _rawHeaderNames = new Map<string, string>();
  private _rawTrailerNames = new Map<string, string>();
  private _informational: SerializedInformationalResponse[] = [];
  private _pendingRawInfoBuffer = "";

  constructor() {
    this._closedPromise = new Promise<void>((resolve) => {
      this._resolveClosed = resolve;
    });
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: EventListener): this {
    const wrapped = (...args: unknown[]): void => {
      this.off(event, wrapped);
      listener(...args);
    };
    return this.on(event, wrapped);
  }

  off(event: string, listener: EventListener): this {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  emit(event: string, ...args: unknown[]): boolean {
    const listeners = this._listeners[event];
    if (!listeners || listeners.length === 0) return false;
    listeners.slice().forEach((fn) => fn(...args));
    return true;
  }

  private _emit(event: string, ...args: unknown[]): void {
    this.emit(event, ...args);
  }

  writeHead(
    statusCode: number,
    headers?: Record<string, string> | Array<[string, string]> | readonly string[]
  ): this {
    if (statusCode >= 100 && statusCode < 200 && statusCode !== 101) {
      const informationalHeaders = new Map<string, StoredHeaderValue>();
      const informationalRawHeaderNames = new Map<string, string>();
      if (headers) {
        if (isFlatHeaderList(headers)) {
          for (let index = 0; index < headers.length; index += 2) {
            const key = headers[index];
            const value = headers[index + 1];
            if (key === undefined || value === undefined) {
              continue;
            }
            const actualName = validateHeaderName(key).toLowerCase();
            validateHeaderValue(actualName, value);
            informationalHeaders.set(actualName, String(value));
            if (!informationalRawHeaderNames.has(actualName)) {
              informationalRawHeaderNames.set(actualName, key);
            }
          }
        } else if (Array.isArray(headers)) {
          headers.forEach(([key, value]) => {
            const actualName = validateHeaderName(key).toLowerCase();
            validateHeaderValue(actualName, value);
            informationalHeaders.set(actualName, String(value));
            if (!informationalRawHeaderNames.has(actualName)) {
              informationalRawHeaderNames.set(actualName, key);
            }
          });
        } else {
          Object.entries(headers).forEach(([key, value]) => {
            const actualName = validateHeaderName(key).toLowerCase();
            validateHeaderValue(actualName, value);
            informationalHeaders.set(actualName, String(value));
            if (!informationalRawHeaderNames.has(actualName)) {
              informationalRawHeaderNames.set(actualName, key);
            }
          });
        }
      }
      const normalizedHeaders = Array.from(informationalHeaders.entries()).flatMap(([key, value]) => {
        const serialized = serializeHeaderValue(value);
        return Array.isArray(serialized)
          ? serialized.map((entry) => [key, entry] as [string, string])
          : [[key, serialized] as [string, string]];
      });
      const rawHeaders = Array.from(informationalHeaders.entries()).flatMap(([key, value]) => {
        const rawName = informationalRawHeaderNames.get(key) || key;
        const serialized = serializeHeaderValue(value);
        return Array.isArray(serialized)
          ? serialized.flatMap((entry) => [rawName, entry])
          : [rawName, serialized];
      });
      this._informational.push({
        status: statusCode,
        statusText: HTTP_STATUS_TEXT[statusCode],
        headers: normalizedHeaders,
        rawHeaders,
      });
      return this;
    }
    this.statusCode = statusCode;
    if (headers) {
      if (isFlatHeaderList(headers)) {
        for (let index = 0; index < headers.length; index += 2) {
          const key = headers[index];
          const value = headers[index + 1];
          if (key !== undefined && value !== undefined) {
            this.setHeader(key, value);
          }
        }
      } else if (Array.isArray(headers)) {
        headers.forEach(([key, value]) => this.setHeader(key, value));
      } else {
        Object.entries(headers).forEach(([key, value]) =>
          this.setHeader(key, value)
        );
      }
    }
    this.headersSent = true;
    this.outputSize += 64;
    return this;
  }

  setHeader(name: string, value: string | number | readonly (string | number)[]): this {
    if (this.headersSent) {
      throw createErrorWithCode(
        "Cannot set headers after they are sent to the client",
        "ERR_HTTP_HEADERS_SENT",
      );
    }
    const lower = validateHeaderName(name).toLowerCase();
    validateHeaderValue(lower, value);
    const storedValue: StoredHeaderValue = Array.isArray(value)
      ? Array.from(value as readonly (string | number)[])
      : value as string | number;
    this._headers.set(lower, storedValue);
    if (!this._rawHeaderNames.has(lower)) {
      this._rawHeaderNames.set(lower, name);
    }
    return this;
  }

  setHeaders(headers: Headers | Map<string, string | string[]>): this {
    if (this.headersSent) {
      throw createErrorWithCode(
        "Cannot set headers after they are sent to the client",
        "ERR_HTTP_HEADERS_SENT",
      );
    }
    if (!(headers instanceof Headers) && !(headers instanceof Map)) {
      throw createTypeErrorWithCode(
        `The "headers" argument must be an instance of Headers or Map. Received ${formatReceivedType(headers)}`,
        "ERR_INVALID_ARG_TYPE",
      );
    }

    if (headers instanceof Headers) {
      const pending = Object.create(null) as Record<string, string | string[]>;
      headers.forEach((value, key) => {
        appendNormalizedHeader(pending, key.toLowerCase(), value);
      });
      Object.entries(pending).forEach(([key, value]) => {
        this.setHeader(key, value);
      });
      return this;
    }

    headers.forEach((value, key) => {
      this.setHeader(key, value);
    });
    return this;
  }

  getHeader(name: string): StoredHeaderValue | undefined {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE",
      );
    }
    const value = this._headers.get(name.toLowerCase());
    return value === undefined ? undefined : cloneStoredHeaderValue(value);
  }

  hasHeader(name: string): boolean {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE",
      );
    }
    return this._headers.has(name.toLowerCase());
  }

  removeHeader(name: string): void {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE",
      );
    }
    const lower = name.toLowerCase();
    this._headers.delete(lower);
    this._rawHeaderNames.delete(lower);
  }

  write(
    chunk: string | Uint8Array | null,
    encodingOrCallback?: BufferEncoding | (() => void),
    callback?: () => void,
  ): boolean {
    if (chunk == null) return true;
    this.headersSent = true;
    const buf =
      typeof chunk === "string"
        ? Buffer.from(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : undefined)
        : chunk;
    if (this._chunksBytes + buf.byteLength > MAX_HTTP_BODY_BYTES) {
      throw new Error("ERR_HTTP_BODY_TOO_LARGE: response body exceeds " + MAX_HTTP_BODY_BYTES + " byte limit");
    }
    this._chunks.push(buf);
    this._chunksBytes += buf.byteLength;
    this.outputSize += buf.byteLength;
    const writeCallback = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    if (typeof writeCallback === "function") {
      queueMicrotask(writeCallback);
    }
    return true;
  }

  end(
    chunkOrCallback?: string | Uint8Array | null | (() => void),
    encodingOrCallback?: BufferEncoding | (() => void),
    callback?: () => void,
  ): this {
    let chunk: string | Uint8Array | null | undefined;
    let endCallback: (() => void) | undefined;

    if (typeof chunkOrCallback === "function") {
      endCallback = chunkOrCallback;
    } else {
      chunk = chunkOrCallback;
      endCallback = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    }

    if (chunk != null) {
      if (typeof chunk === "string" && typeof encodingOrCallback === "string") {
        this.write(Buffer.from(chunk, encodingOrCallback));
      } else {
        this.write(chunk);
      }
    }
    this._finalize();
    if (typeof endCallback === "function") {
      queueMicrotask(endCallback);
    }
    return this;
  }

  getHeaderNames(): string[] {
    return Array.from(this._headers.keys());
  }

  getRawHeaderNames(): string[] {
    return Array.from(this._headers.keys()).map((key) => this._rawHeaderNames.get(key) || key);
  }

  getHeaders(): Record<string, StoredHeaderValue> {
    const result = Object.create(null) as Record<string, StoredHeaderValue>;
    for (const [key, value] of this._headers) {
      result[key] = cloneStoredHeaderValue(value);
    }
    return result;
  }

  // Writable stream state stub for frameworks that inspect internal state
  _writableState = { length: 0, ended: false, finished: false, objectMode: false, corked: 0 };

  // Fake socket for frameworks that access res.socket/res.connection
  socket = {
    writable: true,
    writableCorked: 0,
    writableHighWaterMark: 16 * 1024,
    on: () => this.socket,
    once: () => this.socket,
    removeListener: () => this.socket,
    destroy: () => {
      this._connectionReset = true;
      this._finalize();
    },
    end: () => {
      this._connectionEnded = true;
    },
    cork: () => {
      this._writableState.corked += 1;
      this.socket.writableCorked = this._writableState.corked;
    },
    uncork: () => {
      this._writableState.corked = Math.max(0, this._writableState.corked - 1);
      this.socket.writableCorked = this._writableState.corked;
    },
    write: (_chunk?: unknown, callback?: () => void) => {
      if (typeof callback === "function") {
        queueMicrotask(callback);
      }
      return true;
    },
  } as Record<string, unknown>;
  connection = this.socket;

  // Node.js http.ServerResponse socket/stream compatibility stubs
  assignSocket(): void { /* no-op */ }
  detachSocket(): void { /* no-op */ }
  writeContinue(): void { this.writeHead(100); }
  writeProcessing(): void { this.writeHead(102); }
  addTrailers(headers: Record<string, string> | readonly string[]): void {
    if (Array.isArray(headers)) {
      for (let index = 0; index < headers.length; index += 2) {
        const key = headers[index];
        const value = headers[index + 1];
        if (key === undefined || value === undefined) {
          continue;
        }
        const actualName = validateHeaderName(key).toLowerCase();
        validateHeaderValue(actualName, value);
        this._trailers.set(actualName, String(value));
        if (!this._rawTrailerNames.has(actualName)) {
          this._rawTrailerNames.set(actualName, key);
        }
      }
      return;
    }

    Object.entries(headers).forEach(([key, value]) => {
      const actualName = validateHeaderName(key).toLowerCase();
      validateHeaderValue(actualName, value);
      this._trailers.set(actualName, String(value));
      if (!this._rawTrailerNames.has(actualName)) {
        this._rawTrailerNames.set(actualName, key);
      }
    });
  }
  cork(): void {
    (this.socket.cork as () => void)();
  }
  uncork(): void {
    (this.socket.uncork as () => void)();
  }
  setTimeout(_msecs?: number): this { return this; }
  get writableCorked(): number {
    return Number((this.socket as { writableCorked?: number }).writableCorked || 0);
  }

  flushHeaders(): void {
    this.headersSent = true;
  }

  destroy(err?: Error): void {
    this._connectionReset = true;
    if (err) {
      this._emit("error", err);
    }
    this._finalize();
  }

  async waitForClose(): Promise<void> {
    await this._closedPromise;
  }

  serialize(): SerializedServerResponse {
    const bodyBuffer =
      this._chunks.length > 0 ? Buffer.concat(this._chunks) : Buffer.alloc(0);
    const serializedHeaders = Array.from(this._headers.entries()).flatMap(([key, value]) => {
      const serialized = serializeHeaderValue(value);
      if (Array.isArray(serialized)) {
        if (key === "set-cookie") {
          return serialized.map((entry) => [key, entry] as [string, string]);
        }
        return [[key, serialized.join(", ")] as [string, string]];
      }
      return [[key, serialized] as [string, string]];
    });
    const rawHeaders = Array.from(this._headers.entries()).flatMap(([key, value]) => {
      const rawName = this._rawHeaderNames.get(key) || key;
      const serialized = serializeHeaderValue(value);
      if (Array.isArray(serialized)) {
        if (key === "set-cookie") {
          return serialized.flatMap((entry) => [rawName, entry]);
        }
        return [rawName, serialized.join(", ")];
      }
      return [rawName, serialized];
    });
    const serializedTrailers = Array.from(this._trailers.entries()).flatMap(([key, value]) => {
      const serialized = serializeHeaderValue(value);
      return Array.isArray(serialized)
        ? serialized.map((entry) => [key, entry] as [string, string])
        : [[key, serialized] as [string, string]];
    });
    const rawTrailers = Array.from(this._trailers.entries()).flatMap(([key, value]) => {
      const rawName = this._rawTrailerNames.get(key) || key;
      const serialized = serializeHeaderValue(value);
      return Array.isArray(serialized)
        ? serialized.flatMap((entry) => [rawName, entry])
        : [rawName, serialized];
    });
    return {
      status: this.statusCode,
      headers: serializedHeaders,
      rawHeaders,
      informational: this._informational.length > 0 ? [...this._informational] : undefined,
      body: bodyBuffer.toString("base64"),
      bodyEncoding: "base64",
      trailers: serializedTrailers.length > 0 ? serializedTrailers : undefined,
      rawTrailers: rawTrailers.length > 0 ? rawTrailers : undefined,
      connectionEnded: this._connectionEnded,
      connectionReset: this._connectionReset,
    };
  }

  _writeRaw(chunk: string, callback?: () => void): boolean {
    this._pendingRawInfoBuffer += String(chunk);
    this._flushPendingRawInformational();
    if (typeof callback === "function") {
      queueMicrotask(callback);
    }
    return true;
  }

  private _finalize(): void {
    if (this.writableFinished) {
      return;
    }
    this.writableFinished = true;
    this.writable = false;
    this._writableState.ended = true;
    this._writableState.finished = true;
    this._emit("finish");
    this._emit("close");
    this._resolveClosed?.();
    this._resolveClosed = null;
  }

  private _flushPendingRawInformational(): void {
    let separatorIndex = this._pendingRawInfoBuffer.indexOf("\r\n\r\n");
    while (separatorIndex !== -1) {
      const rawFrame = this._pendingRawInfoBuffer.slice(0, separatorIndex);
      this._pendingRawInfoBuffer = this._pendingRawInfoBuffer.slice(separatorIndex + 4);

      const [statusLine, ...headerLines] = rawFrame.split("\r\n");
      const statusMatch = /^HTTP\/1\.[01]\s+(\d{3})(?:\s+(.*))?$/.exec(statusLine);
      if (!statusMatch) {
        separatorIndex = this._pendingRawInfoBuffer.indexOf("\r\n\r\n");
        continue;
      }

      const status = Number(statusMatch[1]);
      if (status >= 100 && status < 200 && status !== 101) {
        const headers: Array<[string, string]> = [];
        const rawHeaders: string[] = [];
        for (const headerLine of headerLines) {
          const separator = headerLine.indexOf(":");
          if (separator === -1) {
            continue;
          }
          const key = headerLine.slice(0, separator).trim();
          const value = headerLine.slice(separator + 1).trim();
          headers.push([key.toLowerCase(), value]);
          rawHeaders.push(key, value);
        }

        this._informational.push({
          status,
          statusText: statusMatch[2] || HTTP_STATUS_TEXT[status] || undefined,
          headers,
          rawHeaders,
        });
      }

      separatorIndex = this._pendingRawInfoBuffer.indexOf("\r\n\r\n");
    }
  }
}

/**
 * Polyfill of Node.js `http.Server`. Delegates listening through the
 * kernel-backed `_networkHttpServerListenRaw` bridge. Incoming requests are
 * dispatched through `_httpServerDispatch`, which invokes the request listener
 * inside the isolate. Registers an active handle to keep the sandbox alive.
 */
class Server {
  listening = false;
  private _listeners: Record<string, EventListener[]> = {};
  private _serverId: number;
  private _listenPromise: Promise<void> | null = null;
  private _address: ServerAddress | null = null;
  private _handleId: string | null = null;
  private _hostCloseWaitStarted = false;
  private _activeRequestDispatches = 0;
  private _closePending = false;
  private _closeRunning = false;
  private _closeCallbacks: Array<(err?: Error) => void> = [];
  /** @internal Request listener stored on the instance (replaces serverRequestListeners Map). */
  _requestListener: (req: ServerIncomingMessage, res: ServerResponseBridge) => unknown;

  constructor(requestListener?: (req: ServerIncomingMessage, res: ServerResponseBridge) => unknown) {
    this._serverId = nextServerId++;
    this._requestListener = requestListener ?? (() => undefined);
    serverInstances.set(this._serverId, this);
  }

  /** @internal Bridge-visible server ID for loopback self-dispatch. */
  get _bridgeServerId(): number {
    return this._serverId;
  }

  /** @internal Emit an event — used by upgrade dispatch to fire 'upgrade' events. */
  _emit(event: string, ...args: unknown[]): void {
    const listeners = this._listeners[event];
    if (!listeners || listeners.length === 0) return;
    listeners.slice().forEach((listener) => listener(...args));
  }

  private _finishStart(resultJson: string): void {
    const result = JSON.parse(resultJson) as SerializedServerListenResult;
    this._address = result.address;
    this.listening = true;
    this._handleId = `http-server:${this._serverId}`;
    debugBridgeNetwork("server listening", this._serverId, this._address);
    if (typeof _registerHandle === "function") {
      _registerHandle(this._handleId, "http server");
    }
    this._startHostCloseWait();
  }

  private _completeClose(): void {
    this.listening = false;
    this._address = null;
    serverInstances.delete(this._serverId);
    if (this._handleId && typeof _unregisterHandle === "function") {
      _unregisterHandle(this._handleId);
    }
    this._handleId = null;
  }

  _beginRequestDispatch(): void {
    this._activeRequestDispatches += 1;
  }

  _endRequestDispatch(): void {
    this._activeRequestDispatches = Math.max(0, this._activeRequestDispatches - 1);
    if (this._closePending && this._activeRequestDispatches === 0) {
      this._closePending = false;
      queueMicrotask(() => {
        this._startClose();
      });
    }
  }

  private _startHostCloseWait(): void {
    if (this._hostCloseWaitStarted || typeof _networkHttpServerWaitRaw === "undefined") {
      return;
    }
    this._hostCloseWaitStarted = true;
    void _networkHttpServerWaitRaw
      .apply(undefined, [this._serverId], { result: { promise: true } })
      .then(() => {
        if (!this.listening) {
          return;
        }
        debugBridgeNetwork("server close from host", this._serverId);
        this._completeClose();
        this._emit("close");
      })
      .catch(() => {
        // Ignore shutdown races during teardown.
      });
  }

  private async _start(port?: number, hostname?: string): Promise<void> {
    if (typeof _networkHttpServerListenRaw === "undefined") {
      throw new Error(
        "http.createServer requires kernel-backed network bridge support"
      );
    }

    debugBridgeNetwork("server listen start", this._serverId, port, hostname);
    const resultJson = await _networkHttpServerListenRaw.apply(
      undefined,
      [JSON.stringify({ serverId: this._serverId, port, hostname })],
      { result: { promise: true } }
    );
    this._finishStart(resultJson);
  }

  listen(
    portOrCb?: number | (() => void),
    hostOrCb?: string | (() => void),
    cb?: () => void
  ): this {
    const port = typeof portOrCb === "number" ? portOrCb : undefined;
    const hostname = typeof hostOrCb === "string" ? hostOrCb : undefined;
    const callback =
      typeof cb === "function"
        ? cb
        : typeof hostOrCb === "function"
          ? hostOrCb
          : typeof portOrCb === "function"
            ? portOrCb
            : undefined;

    if (!this._listenPromise) {
      this._listenPromise = this._start(port, hostname)
        .then(() => {
          this._emit("listening");
          callback?.call(this);
        })
        .catch((error) => {
          this._emit("error", error);
        });
    }
    return this;
  }

  close(cb?: (err?: Error) => void): this {
    debugBridgeNetwork("server close requested", this._serverId, this.listening);
    if (cb) {
      this._closeCallbacks.push(cb);
    }
    if (this._activeRequestDispatches > 0) {
      this._closePending = true;
      return this;
    }
    queueMicrotask(() => {
      this._startClose();
    });
    return this;
  }

  private _startClose(): void {
    if (this._closeRunning) {
      return;
    }
    this._closeRunning = true;
    const run = async () => {
      try {
        if (this._listenPromise) {
          await this._listenPromise;
        }
        if (this.listening && typeof _networkHttpServerCloseRaw !== "undefined") {
          debugBridgeNetwork("server close bridge call", this._serverId);
          await _networkHttpServerCloseRaw.apply(undefined, [this._serverId], {
            result: { promise: true },
          });
        }
        this._completeClose();
        debugBridgeNetwork("server close complete", this._serverId);
        const callbacks = this._closeCallbacks.splice(0);
        callbacks.forEach((callback) => callback());
        this._emit("close");
      } catch (err) {
        const error = err instanceof Error ? err : new Error(String(err));
        debugBridgeNetwork("server close error", this._serverId, error.message);
        const callbacks = this._closeCallbacks.splice(0);
        callbacks.forEach((callback) => callback(error));
        this._emit("error", error);
      } finally {
        this._closeRunning = false;
      }
    };
    void run();
  }

  address(): ServerAddress | null {
    return this._address;
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: EventListener): this {
    const wrapped = (...args: unknown[]): void => {
      this.off(event, wrapped);
      listener(...args);
    };
    return this.on(event, wrapped);
  }

  off(event: string, listener: EventListener): this {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  removeAllListeners(event?: string): this {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }

  listenerCount(event: string): number {
    return this._listeners[event]?.length || 0;
  }

  // Node.js Server timeout properties (no-op in sandbox)
  keepAliveTimeout = 5000;
  requestTimeout = 300000;
  headersTimeout = 60000;
  timeout = 0;
  maxRequestsPerSocket = 0;

  setTimeout(_msecs?: number, _callback?: () => void): this {
    if (typeof _msecs === "number") this.timeout = _msecs;
    return this;
  }

  ref(): this {
    return this;
  }

  unref(): this {
    return this;
  }
}

// Function-style Server constructor for code that calls http.Server(...)
// without `new`, matching the callable shape Node exposes.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function ServerCallable(this: any, requestListener?: (req: ServerIncomingMessage, res: ServerResponseBridge) => unknown): Server {
  return new Server(requestListener);
}
ServerCallable.prototype = Server.prototype;

/** Route an incoming HTTP request to the server's request listener and return the serialized response. */
async function dispatchServerRequest(
  serverId: number,
  requestJson: string
): Promise<string> {
  const server = serverInstances.get(serverId);
  if (!server) {
    throw new Error(`Unknown HTTP server: ${serverId}`);
  }
  const listener = server._requestListener;
  server._beginRequestDispatch();

  const request = JSON.parse(requestJson) as SerializedServerRequest;
  const incoming = new ServerIncomingMessage(request);
  const outgoing = new ServerResponseBridge();
  incoming.socket = outgoing.socket;
  incoming.connection = outgoing.socket;
  const pendingImmediates: Promise<void>[] = [];
  const pendingTimers: Promise<void>[] = [];
  const trackedTimers = new Map<ReturnType<typeof setTimeout>, () => void>();
  let consumedTimerCount = 0;
  let consumedImmediateCount = 0;

  try {
    try {
      const originalSetImmediate = globalThis.setImmediate;
      const originalSetTimeout = globalThis.setTimeout;
      const originalClearTimeout = globalThis.clearTimeout;
      if (typeof originalSetImmediate === "function") {
        globalThis.setImmediate = ((
          callback: (...args: unknown[]) => unknown,
          ...args: unknown[]
        ) => {
          const pending = new Promise<void>((resolve) => {
            queueMicrotask(() => {
              try {
                callback(...args);
              } finally {
                resolve();
              }
            });
          });
          pendingImmediates.push(pending);
          return 0 as unknown as ReturnType<typeof setImmediate>;
        }) as typeof setImmediate;
      }
      if (typeof originalSetTimeout === "function") {
        globalThis.setTimeout = ((
          callback: (...args: unknown[]) => unknown,
          delay?: number,
          ...args: unknown[]
        ) => {
          if (typeof callback !== "function") {
            return originalSetTimeout(callback as TimerHandler, delay, ...args);
          }

          const normalizedDelay =
            typeof delay === "number" && Number.isFinite(delay)
              ? Math.max(0, delay)
              : 0;

          if (normalizedDelay > 1_000) {
            return originalSetTimeout(callback, normalizedDelay, ...args);
          }

          let resolvePending!: () => void;
          const pending = new Promise<void>((resolve) => {
            resolvePending = resolve;
          });
          let handle: ReturnType<typeof setTimeout>;
          handle = originalSetTimeout(() => {
            trackedTimers.delete(handle);
            try {
              callback(...args);
            } finally {
              resolvePending();
            }
          }, normalizedDelay);
          trackedTimers.set(handle, resolvePending);
          pendingTimers.push(pending);
          return handle;
        }) as typeof setTimeout;
      }
      if (typeof originalClearTimeout === "function") {
        globalThis.clearTimeout = ((handle?: ReturnType<typeof setTimeout>) => {
          if (handle != null) {
            const resolvePending = trackedTimers.get(handle);
            if (resolvePending) {
              trackedTimers.delete(handle);
              resolvePending();
            }
          }
          return originalClearTimeout(handle);
        }) as typeof clearTimeout;
      }

      try {
        // Call listener synchronously — frameworks register event handlers here
        const listenerResult = listener(incoming, outgoing);

        // Emit readable stream events so body-parsing middleware (e.g. express.json()) can proceed
        if (incoming.rawBody && incoming.rawBody.length > 0) {
          incoming.emit("data", incoming.rawBody);
        }
        incoming.emit("end");

        await Promise.resolve(listenerResult);
        while (
          consumedTimerCount < pendingTimers.length ||
          consumedImmediateCount < pendingImmediates.length
        ) {
          const pending = [
            ...pendingTimers.slice(consumedTimerCount),
            ...pendingImmediates.slice(consumedImmediateCount),
          ];
          consumedTimerCount = pendingTimers.length;
          consumedImmediateCount = pendingImmediates.length;
          await Promise.allSettled(pending);
        }
      } finally {
        if (typeof originalSetImmediate === "function") {
          globalThis.setImmediate = originalSetImmediate;
        }
        if (typeof originalSetTimeout === "function") {
          globalThis.setTimeout = originalSetTimeout;
        }
        if (typeof originalClearTimeout === "function") {
          globalThis.clearTimeout = originalClearTimeout;
        }
      }
    } catch (err) {
      outgoing.statusCode = 500;
      try {
        outgoing.end(err instanceof Error ? `Error: ${err.message}` : "Error");
      } catch {
        // Body cap may prevent writing error — finalize without data
        if (!outgoing.writableFinished) outgoing.end();
      }
    }

    if (!outgoing.writableFinished) {
      outgoing.end();
    }

    await outgoing.waitForClose();
    await Promise.allSettled([...pendingTimers, ...pendingImmediates]);
    return JSON.stringify(outgoing.serialize());
  } finally {
    server._endRequestDispatch();
  }
}

async function dispatchHttp2CompatibilityRequest(
  serverId: number,
  requestId: number,
): Promise<void> {
  const pending = pendingHttp2CompatRequests.get(requestId);
  if (!pending || pending.serverId !== serverId || typeof _networkHttp2ServerRespondRaw === "undefined") {
    return;
  }
  pendingHttp2CompatRequests.delete(requestId);

  const server = http2Servers.get(serverId);
  if (!server) {
    _networkHttp2ServerRespondRaw.applySync(undefined, [
      serverId,
      requestId,
      JSON.stringify({
        status: 500,
        headers: [["content-type", "text/plain"]],
        body: "Unknown HTTP/2 server",
        bodyEncoding: "utf8",
      }),
    ]);
    return;
  }

  const request = JSON.parse(pending.requestJson) as SerializedServerRequest;
  const incoming = new ServerIncomingMessage(request);
  const outgoing = new ServerResponseBridge();
  incoming.socket = outgoing.socket;
  incoming.connection = outgoing.socket;

  try {
    server.emit("request", incoming, outgoing);
    if (incoming.rawBody && incoming.rawBody.length > 0) {
      incoming.emit("data", incoming.rawBody);
    }
    incoming.emit("end");
    if (!outgoing.writableFinished) {
      outgoing.end();
    }
    await outgoing.waitForClose();
    _networkHttp2ServerRespondRaw.applySync(undefined, [
      serverId,
      requestId,
      JSON.stringify(outgoing.serialize()),
    ]);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    _networkHttp2ServerRespondRaw.applySync(undefined, [
      serverId,
      requestId,
      JSON.stringify({
        status: 500,
        headers: [["content-type", "text/plain"]],
        body: `Error: ${message}`,
        bodyEncoding: "utf8",
      }),
    ]);
  }
}

async function dispatchLoopbackServerRequest(
  serverOrId: number | Server,
  requestInput: string | SerializedServerRequest,
): Promise<{
  responseJson: string;
  abortRequest: () => void;
}> {
  const server =
    typeof serverOrId === "number"
      ? serverInstances.get(serverOrId)
      : serverOrId;
  if (!server) {
    throw new Error(
      `Unknown HTTP server: ${typeof serverOrId === "number" ? serverOrId : "<detached>"}`,
    );
  }

  const request =
    typeof requestInput === "string"
      ? JSON.parse(requestInput) as SerializedServerRequest
      : requestInput;
  const incoming = new ServerIncomingMessage(request);
  const outgoing = new ServerResponseBridge();
  incoming.socket = outgoing.socket;
  incoming.connection = outgoing.socket;
  const pendingImmediates: Promise<void>[] = [];
  const pendingTimers: Promise<void>[] = [];
  const trackedTimers = new Map<ReturnType<typeof setTimeout>, () => void>();
  let consumedTimerCount = 0;
  let consumedImmediateCount = 0;
  server._beginRequestDispatch();

  try {
    try {
      const originalSetImmediate = globalThis.setImmediate;
      const originalSetTimeout = globalThis.setTimeout;
      const originalClearTimeout = globalThis.clearTimeout;
      if (typeof originalSetImmediate === "function") {
        globalThis.setImmediate = ((
          callback: (...args: unknown[]) => unknown,
          ...args: unknown[]
        ) => {
          const pending = new Promise<void>((resolve) => {
            queueMicrotask(() => {
              try {
                callback(...args);
              } finally {
                resolve();
              }
            });
          });
          pendingImmediates.push(pending);
          return 0 as unknown as ReturnType<typeof setImmediate>;
        }) as typeof setImmediate;
      }
      if (typeof originalSetTimeout === "function") {
        globalThis.setTimeout = ((
          callback: (...args: unknown[]) => unknown,
          delay?: number,
          ...args: unknown[]
        ) => {
          if (typeof callback !== "function") {
            return originalSetTimeout(callback as TimerHandler, delay, ...args);
          }

          const normalizedDelay =
            typeof delay === "number" && Number.isFinite(delay)
              ? Math.max(0, delay)
              : 0;

          if (normalizedDelay > 1_000) {
            return originalSetTimeout(callback, normalizedDelay, ...args);
          }

          let resolvePending!: () => void;
          const pending = new Promise<void>((resolve) => {
            resolvePending = resolve;
          });
          let handle: ReturnType<typeof setTimeout>;
          handle = originalSetTimeout(() => {
            trackedTimers.delete(handle);
            try {
              callback(...args);
            } finally {
              resolvePending();
            }
          }, normalizedDelay);
          trackedTimers.set(handle, resolvePending);
          pendingTimers.push(pending);
          return handle;
        }) as typeof setTimeout;
      }
      if (typeof originalClearTimeout === "function") {
        globalThis.clearTimeout = ((handle?: ReturnType<typeof setTimeout>) => {
          if (handle != null) {
            const resolvePending = trackedTimers.get(handle);
            if (resolvePending) {
              trackedTimers.delete(handle);
              resolvePending();
            }
          }
          return originalClearTimeout(handle);
        }) as typeof clearTimeout;
      }

      try {
        const listenerResult = server._requestListener(incoming, outgoing);

        if (incoming.rawBody && incoming.rawBody.length > 0) {
          incoming.emit("data", incoming.rawBody);
        }
        incoming.emit("end");

        await Promise.resolve(listenerResult);
        while (
          consumedTimerCount < pendingTimers.length ||
          consumedImmediateCount < pendingImmediates.length
        ) {
          const pending = [
            ...pendingTimers.slice(consumedTimerCount),
            ...pendingImmediates.slice(consumedImmediateCount),
          ];
          consumedTimerCount = pendingTimers.length;
          consumedImmediateCount = pendingImmediates.length;
          await Promise.allSettled(pending);
        }
      } finally {
        if (typeof originalSetImmediate === "function") {
          globalThis.setImmediate = originalSetImmediate;
        }
        if (typeof originalSetTimeout === "function") {
          globalThis.setTimeout = originalSetTimeout;
        }
        if (typeof originalClearTimeout === "function") {
          globalThis.clearTimeout = originalClearTimeout;
        }
      }
    } catch (err) {
      outgoing.statusCode = 500;
      try {
        outgoing.end(err instanceof Error ? `Error: ${err.message}` : "Error");
      } catch {
        if (!outgoing.writableFinished) outgoing.end();
      }
    }

    if (!outgoing.writableFinished) {
      outgoing.end();
    }

    await outgoing.waitForClose();
    await Promise.allSettled([...pendingTimers, ...pendingImmediates]);
    let aborted = false;
    return {
      responseJson: JSON.stringify(outgoing.serialize()),
      abortRequest: () => {
        if (aborted) {
          return;
        }
        aborted = true;
        incoming._abort();
      },
    };
  } finally {
    server._endRequestDispatch();
  }
}

async function dispatchLoopbackConnectRequest(
  server: Server,
  options: nodeHttp.RequestOptions,
): Promise<{
  response: IncomingMessage;
  socket: DirectTunnelSocket;
  head: Buffer;
}> {
  return await new Promise((resolve, reject) => {
    const request = new ServerIncomingMessage({
      method: "CONNECT",
      url: String(options.path || "/"),
      headers: normalizeRequestHeaders(options.headers),
      rawHeaders: flattenRawHeaders(normalizeRequestHeaders(options.headers)),
    });
    const clientSocket = new DirectTunnelSocket({
      host: String(options.hostname || options.host || "127.0.0.1"),
      port: Number(options.port) || 80,
    });
    const serverSocket = new DirectTunnelSocket({
      host: "127.0.0.1",
      port: 0,
    });
    clientSocket._attachPeer(serverSocket);
    serverSocket._attachPeer(clientSocket);

    const originalWrite = serverSocket.write.bind(serverSocket);
    const originalEnd = serverSocket.end.bind(serverSocket);
    let handshakeBuffer = Buffer.alloc(0);
    let handshakeResolved = false;

    const maybeResolveHandshake = (): void => {
      if (handshakeResolved) {
        return;
      }

      const separator = handshakeBuffer.indexOf("\r\n\r\n");
      if (separator === -1) {
        return;
      }

      const headerBuffer = handshakeBuffer.subarray(0, separator);
      const head = handshakeBuffer.subarray(separator + 4);
      const [statusLine, ...headerLines] = headerBuffer.toString("latin1").split("\r\n");
      const statusMatch = /^HTTP\/1\.[01]\s+(\d{3})(?:\s+(.*))?$/.exec(statusLine);
      if (!statusMatch) {
        reject(new Error(`Invalid CONNECT response: ${statusLine}`));
        return;
      }

      handshakeResolved = true;
      const headers: Record<string, string> = {};
      const rawHeaders: string[] = [];
      for (const headerLine of headerLines) {
        const separatorIndex = headerLine.indexOf(":");
        if (separatorIndex === -1) {
          continue;
        }
        const key = headerLine.slice(0, separatorIndex).trim();
        const value = headerLine.slice(separatorIndex + 1).trim();
        headers[key.toLowerCase()] = value;
        rawHeaders.push(key, value);
      }

      resolve({
        response: new IncomingMessage({
          headers,
          rawHeaders,
          status: Number(statusMatch[1]),
          statusText: statusMatch[2] || HTTP_STATUS_TEXT[Number(statusMatch[1])],
        }),
        socket: clientSocket,
        head,
      });
    };

    serverSocket.write = ((data: unknown, encodingOrCb?: string | (() => void), cb?: (() => void)) => {
      if (handshakeResolved) {
        return originalWrite(data, encodingOrCb as string, cb);
      }
      const callback = typeof encodingOrCb === "function" ? encodingOrCb : cb;
      handshakeBuffer = Buffer.concat([handshakeBuffer, normalizeSocketChunk(data)]);
      maybeResolveHandshake();
      callback?.();
      return true;
    }) as typeof serverSocket.write;

    serverSocket.end = ((data?: unknown) => {
      if (data !== undefined) {
        serverSocket.write(data);
      }
      if (!handshakeResolved) {
        maybeResolveHandshake();
      }
      return originalEnd();
    }) as typeof serverSocket.end;

    try {
      server._emit("connect", request, serverSocket, Buffer.alloc(0));
    } catch (error) {
      reject(error instanceof Error ? error : new Error(String(error)));
      return;
    }

    queueMicrotask(() => {
      if (!handshakeResolved) {
        reject(new Error("Loopback CONNECT handler did not establish a tunnel"));
      }
    });
  });
}

async function dispatchLoopbackUpgradeRequest(
  server: Server,
  options: nodeHttp.RequestOptions,
  requestBody?: string,
): Promise<{
  response: IncomingMessage;
  socket: DirectTunnelSocket;
  head: Buffer;
}> {
  return await new Promise((resolve, reject) => {
    const normalizedHeaders = normalizeRequestHeaders(options.headers);
    const request = new ServerIncomingMessage({
      method: String(options.method || "GET").toUpperCase(),
      url: String(options.path || "/"),
      headers: normalizedHeaders,
      rawHeaders: flattenRawHeaders(normalizedHeaders),
      bodyBase64: requestBody
        ? Buffer.from(requestBody).toString("base64")
        : undefined,
    });
    const clientSocket = new DirectTunnelSocket({
      host: String(options.hostname || options.host || "127.0.0.1"),
      port: Number(options.port) || 80,
    });
    const serverSocket = new DirectTunnelSocket({
      host: "127.0.0.1",
      port: 0,
    });
    clientSocket._attachPeer(serverSocket);
    serverSocket._attachPeer(clientSocket);

    const originalWrite = serverSocket.write.bind(serverSocket);
    const originalEnd = serverSocket.end.bind(serverSocket);
    let handshakeBuffer = Buffer.alloc(0);
    let handshakeResolved = false;

    const maybeResolveHandshake = (): void => {
      if (handshakeResolved) {
        return;
      }

      const separator = handshakeBuffer.indexOf("\r\n\r\n");
      if (separator === -1) {
        return;
      }

      const headerBuffer = handshakeBuffer.subarray(0, separator);
      const head = handshakeBuffer.subarray(separator + 4);
      const [statusLine, ...headerLines] = headerBuffer.toString("latin1").split("\r\n");
      const statusMatch = /^HTTP\/1\.[01]\s+(\d{3})(?:\s+(.*))?$/.exec(statusLine);
      if (!statusMatch) {
        reject(new Error(`Invalid upgrade response: ${statusLine}`));
        return;
      }

      handshakeResolved = true;
      const headers: Record<string, string> = {};
      const rawHeaders: string[] = [];
      for (const headerLine of headerLines) {
        const separatorIndex = headerLine.indexOf(":");
        if (separatorIndex === -1) {
          continue;
        }
        const key = headerLine.slice(0, separatorIndex).trim();
        const value = headerLine.slice(separatorIndex + 1).trim();
        headers[key.toLowerCase()] = value;
        rawHeaders.push(key, value);
      }

      resolve({
        response: new IncomingMessage({
          headers,
          rawHeaders,
          status: Number(statusMatch[1]),
          statusText: statusMatch[2] || HTTP_STATUS_TEXT[Number(statusMatch[1])],
        }),
        socket: clientSocket,
        head,
      });
    };

    serverSocket.write = ((data: unknown, encodingOrCb?: string | (() => void), cb?: (() => void)) => {
      if (handshakeResolved) {
        return originalWrite(data, encodingOrCb as string, cb);
      }
      const callback = typeof encodingOrCb === "function" ? encodingOrCb : cb;
      handshakeBuffer = Buffer.concat([handshakeBuffer, normalizeSocketChunk(data)]);
      maybeResolveHandshake();
      callback?.();
      return true;
    }) as typeof serverSocket.write;

    serverSocket.end = ((data?: unknown) => {
      if (data !== undefined) {
        serverSocket.write(data);
      }
      if (!handshakeResolved) {
        maybeResolveHandshake();
      }
      return originalEnd();
    }) as typeof serverSocket.end;

    try {
      server._emit(
        "upgrade",
        request,
        serverSocket,
        request.rawBody || Buffer.alloc(0),
      );
    } catch (error) {
      reject(error instanceof Error ? error : new Error(String(error)));
      return;
    }

    queueMicrotask(() => {
      if (!handshakeResolved) {
        reject(new Error("Loopback upgrade handler did not establish a protocol switch"));
      }
    });
  });
}

function dispatchSocketRequest(
  event: "upgrade" | "connect",
  serverId: number,
  requestJson: string,
  headBase64: string,
  socketId: number,
): void {
  const server = serverInstances.get(serverId);
  if (!server) {
    throw new Error(`Unknown HTTP server for ${event}: ${serverId}`);
  }

  const request = JSON.parse(requestJson) as SerializedServerRequest;
  const incoming = new ServerIncomingMessage(request);
  const head = typeof Buffer !== "undefined" ? Buffer.from(headBase64, "base64") : new Uint8Array(0);
  const hostHeader = incoming.headers["host"];

  const socket = new UpgradeSocket(socketId, {
    host: (
      Array.isArray(hostHeader) ? hostHeader[0] : hostHeader
    )?.split(":")[0] || "127.0.0.1",
  });
  upgradeSocketInstances.set(socketId, socket);
  server._emit(event, incoming, socket, head);
}

// Upgrade socket for bidirectional data relay through the host bridge
const upgradeSocketInstances = new Map<number, UpgradeSocket>();

class UpgradeSocket {
  remoteAddress: string;
  remotePort: number;
  localAddress = "127.0.0.1";
  localPort = 0;
  connecting = false;
  destroyed = false;
  writable = true;
  readable = true;
  readyState = "open";
  bytesWritten = 0;
  private _listeners: Record<string, EventListener[]> = {};
  private _socketId: number;

  // Readable stream state stub for ws compatibility (socketOnClose checks _readableState.endEmitted)
  _readableState = { endEmitted: false };
  _writableState = { finished: false, errorEmitted: false };

  constructor(socketId: number, options?: { host?: string; port?: number }) {
    this._socketId = socketId;
    this.remoteAddress = options?.host || "127.0.0.1";
    this.remotePort = options?.port || 80;
  }

  setTimeout(_ms: number, _cb?: () => void): this { return this; }
  setNoDelay(_noDelay?: boolean): this { return this; }
  setKeepAlive(_enable?: boolean, _delay?: number): this { return this; }
  ref(): this { return this; }
  unref(): this { return this; }
  cork(): void {}
  uncork(): void {}
  pause(): this { return this; }
  resume(): this { return this; }
  address(): { address: string; family: string; port: number } {
    return { address: this.localAddress, family: "IPv4", port: this.localPort };
  }

  on(event: string, listener: EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  addListener(event: string, listener: EventListener): this {
    return this.on(event, listener);
  }

  once(event: string, listener: EventListener): this {
    const wrapper = (...args: unknown[]): void => {
      this.off(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }

  off(event: string, listener: EventListener): this {
    if (this._listeners[event]) {
      const idx = this._listeners[event].indexOf(listener);
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }

  removeListener(event: string, listener: EventListener): this {
    return this.off(event, listener);
  }

  removeAllListeners(event?: string): this {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }

  emit(event: string, ...args: unknown[]): boolean {
    const handlers = this._listeners[event];
    if (handlers) handlers.slice().forEach((fn) => fn.call(this, ...args));
    return handlers !== undefined && handlers.length > 0;
  }

  listenerCount(event: string): number {
    return this._listeners[event]?.length || 0;
  }

  // Allow arbitrary property assignment (used by ws for Symbol properties)
  [key: string | symbol]: unknown;

  write(data: unknown, encodingOrCb?: string | (() => void), cb?: (() => void)): boolean {
    if (this.destroyed) return false;
    const callback = typeof encodingOrCb === "function" ? encodingOrCb : cb;
    if (typeof _upgradeSocketWriteRaw !== "undefined") {
      let base64: string;
      if (typeof Buffer !== "undefined" && Buffer.isBuffer(data)) {
        base64 = data.toString("base64");
      } else if (typeof data === "string") {
        base64 = typeof Buffer !== "undefined" ? Buffer.from(data).toString("base64") : btoa(data);
      } else if (data instanceof Uint8Array) {
        base64 = typeof Buffer !== "undefined" ? Buffer.from(data).toString("base64") : btoa(String.fromCharCode(...data));
      } else {
        base64 = typeof Buffer !== "undefined" ? Buffer.from(String(data)).toString("base64") : btoa(String(data));
      }
      this.bytesWritten += base64.length;
      _upgradeSocketWriteRaw.applySync(undefined, [this._socketId, base64]);
    }
    if (callback) callback();
    return true;
  }

  end(data?: unknown): this {
    if (data) this.write(data);
    if (typeof _upgradeSocketEndRaw !== "undefined" && !this.destroyed) {
      _upgradeSocketEndRaw.applySync(undefined, [this._socketId]);
    }
    this.writable = false;
    this.emit("finish");
    return this;
  }

  destroy(err?: Error): this {
    if (this.destroyed) return this;
    this.destroyed = true;
    this.writable = false;
    this.readable = false;
    this._readableState.endEmitted = true;
    this._writableState.finished = true;
    if (typeof _upgradeSocketDestroyRaw !== "undefined") {
      _upgradeSocketDestroyRaw.applySync(undefined, [this._socketId]);
    }
    upgradeSocketInstances.delete(this._socketId);
    if (err) this.emit("error", err);
    this.emit("close", false);
    return this;
  }

  // Push data received from the host into this socket
  _pushData(data: Buffer | Uint8Array): void {
    this.emit("data", data);
  }

  // Signal end-of-stream from the host
  _pushEnd(): void {
    this.readable = false;
    this._readableState.endEmitted = true;
    this._writableState.finished = true;
    this.emit("end");
    this.emit("close", false);
    upgradeSocketInstances.delete(this._socketId);
  }
}

/** Route an incoming HTTP upgrade to the server's 'upgrade' event listeners. */
function dispatchUpgradeRequest(
  serverId: number,
  requestJson: string,
  headBase64: string,
  socketId: number
): void {
  dispatchSocketRequest("upgrade", serverId, requestJson, headBase64, socketId);
}

/** Route an incoming HTTP CONNECT to the server's 'connect' event listeners. */
function dispatchConnectRequest(
  serverId: number,
  requestJson: string,
  headBase64: string,
  socketId: number
): void {
  dispatchSocketRequest("connect", serverId, requestJson, headBase64, socketId);
}

/** Push data from host to an upgrade socket. */
function onUpgradeSocketData(socketId: number, dataBase64: string): void {
  const socket = upgradeSocketInstances.get(socketId);
  if (socket) {
    const data = typeof Buffer !== "undefined" ? Buffer.from(dataBase64, "base64") : new Uint8Array(0);
    socket._pushData(data);
  }
}

/** Signal end-of-stream from host to an upgrade socket. */
function onUpgradeSocketEnd(socketId: number): void {
  const socket = upgradeSocketInstances.get(socketId);
  if (socket) {
    socket._pushEnd();
  }
}

// Function-based ServerResponse constructor — allows .call() inheritance
// used by light-my-request (Fastify's inject), which does
// http.ServerResponse.call(this, req) + util.inherits(Response, http.ServerResponse)
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function ServerResponseCallable(this: any): void {
  this.statusCode = 200;
  this.statusMessage = "OK";
  this.headersSent = false;
  this.writable = true;
  this.writableFinished = false;
  this.outputSize = 0;
  this._headers = new Map<string, string>();
  this._trailers = new Map<string, string>();
  this._rawHeaderNames = new Map<string, string>();
  this._rawTrailerNames = new Map<string, string>();
  this._informational = [];
  this._pendingRawInfoBuffer = "";
  this._chunks = [] as Uint8Array[];
  this._chunksBytes = 0;
  this._listeners = {} as Record<string, EventListener[]>;
  this._closedPromise = new Promise<void>((resolve) => {
    this._resolveClosed = resolve;
  });
  this._connectionEnded = false;
  this._connectionReset = false;
  // Writable stream state stub
  this._writableState = { length: 0, ended: false, finished: false, objectMode: false, corked: 0 };
  // Fake socket for frameworks/inject libraries that access res.socket
  const fakeSocket = {
    writable: true,
    writableCorked: 0,
    writableHighWaterMark: 16 * 1024,
    on() { return fakeSocket; },
    once() { return fakeSocket; },
    removeListener() { return fakeSocket; },
    destroy() {},
    end() {},
    cork() {},
    uncork() {},
    write() { return true; },
  };
  this.socket = fakeSocket;
  this.connection = fakeSocket;
}
ServerResponseCallable.prototype = Object.create(ServerResponseBridge.prototype, {
  constructor: { value: ServerResponseCallable, writable: true, configurable: true },
});

// Create HTTP module
function createHttpModule(protocol: string): Record<string, unknown> {
  const defaultProtocol = protocol === "https" ? "https:" : "http:";
  const moduleAgent = new Agent({ keepAlive: false });
  // Set module-level globalAgent so ClientRequest defaults to it
  _moduleGlobalAgent = moduleAgent;

  // Ensure protocol is set on request options (defaults to module protocol)
  function ensureProtocol(opts: nodeHttp.RequestOptions): nodeHttp.RequestOptions {
    if (!opts.protocol) return { ...opts, protocol: defaultProtocol };
    return opts;
  }

  return {
    request(
      options: string | URL | nodeHttp.RequestOptions,
      optionsOrCallback?: nodeHttp.RequestOptions | ((res: IncomingMessage) => void),
      maybeCallback?: (res: IncomingMessage) => void,
    ): ClientRequest {
      let opts: nodeHttp.RequestOptions;
      const callback =
        typeof optionsOrCallback === "function"
          ? optionsOrCallback
          : maybeCallback;
      if (typeof options === "string") {
        const url = new URL(options);
        opts = {
          protocol: url.protocol,
          hostname: url.hostname,
          port: url.port,
          path: url.pathname + url.search,
          ...(typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}),
        };
      } else if (options instanceof URL) {
        opts = {
          protocol: options.protocol,
          hostname: options.hostname,
          port: options.port,
          path: options.pathname + options.search,
          ...(typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}),
        };
      } else {
        opts = {
          ...options,
          ...(typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}),
        };
      }
      return new ClientRequest(ensureProtocol(opts), callback as (res: IncomingMessage) => void);
    },

    get(
      options: string | URL | nodeHttp.RequestOptions,
      optionsOrCallback?: nodeHttp.RequestOptions | ((res: IncomingMessage) => void),
      maybeCallback?: (res: IncomingMessage) => void,
    ): ClientRequest {
      let opts: nodeHttp.RequestOptions;
      const callback =
        typeof optionsOrCallback === "function"
          ? optionsOrCallback
          : maybeCallback;
      if (typeof options === "string") {
        const url = new URL(options);
        opts = {
          protocol: url.protocol,
          hostname: url.hostname,
          port: url.port,
          path: url.pathname + url.search,
          method: "GET",
          ...(typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}),
        };
      } else if (options instanceof URL) {
        opts = {
          protocol: options.protocol,
          hostname: options.hostname,
          port: options.port,
          path: options.pathname + options.search,
          method: "GET",
          ...(typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}),
        };
      } else {
        opts = {
          ...options,
          ...(typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}),
          method: "GET",
        };
      }
      const req = new ClientRequest(ensureProtocol(opts), callback as (res: IncomingMessage) => void);
      req.end();
      return req;
    },

    createServer(
      _optionsOrListener?: unknown,
      maybeListener?: (req: ServerIncomingMessage, res: ServerResponseBridge) => void
    ): Server {
      const listener =
        typeof _optionsOrListener === "function"
          ? (_optionsOrListener as (
              req: ServerIncomingMessage,
              res: ServerResponseBridge
            ) => void)
          : maybeListener;
      return new Server(listener);
    },

    Agent,
    globalAgent: moduleAgent,
    Server: ServerCallable as unknown as typeof nodeHttp.Server,
    ServerResponse: ServerResponseCallable as unknown as typeof nodeHttp.ServerResponse,
    IncomingMessage: IncomingMessage as unknown as typeof nodeHttp.IncomingMessage,
    ClientRequest: ClientRequest as unknown as typeof nodeHttp.ClientRequest,
    validateHeaderName,
    validateHeaderValue,
    _checkIsHttpToken: checkIsHttpToken,
    _checkInvalidHeaderChar: checkInvalidHeaderChar,
    METHODS: [...HTTP_METHODS],
    STATUS_CODES: HTTP_STATUS_TEXT,
  };
}

async function dispatchLoopbackHttp2CompatibilityRequest(
  server: Http2Server,
  requestInput: string | SerializedServerRequest,
): Promise<{
  responseJson: string;
  abortRequest: () => void;
}> {
  const request =
    typeof requestInput === "string"
      ? JSON.parse(requestInput) as SerializedServerRequest
      : requestInput;
  const incoming = new ServerIncomingMessage(request);
  const outgoing = new ServerResponseBridge();
  incoming.socket = outgoing.socket;
  incoming.connection = outgoing.socket;

  server.emit("request", incoming, outgoing);
  if (incoming.rawBody && incoming.rawBody.length > 0) {
    incoming.emit("data", incoming.rawBody);
  }
  incoming.emit("end");
  if (!outgoing.writableFinished) {
    outgoing.end();
  }
  await outgoing.waitForClose();

  return {
    responseJson: JSON.stringify(outgoing.serialize()),
    abortRequest: () => incoming._abort(),
  };
}

export const http = createHttpModule("http");
export const https = createHttpModule("https");
const HTTP2_K_SOCKET = Symbol.for("secure-exec.http2.kSocket");
const HTTP2_OPTIONS = Symbol("options");
type Http2HeaderValue = string | string[] | number;
type Http2HeadersRecord = Record<string, Http2HeaderValue>;
type Http2SettingsRecord = Record<string, boolean | number | Record<number, number>>;
type Http2SessionRuntimeState = {
  effectiveLocalWindowSize?: number;
  localWindowSize?: number;
  remoteWindowSize?: number;
  nextStreamID?: number;
  outboundQueueSize?: number;
  deflateDynamicTableSize?: number;
  inflateDynamicTableSize?: number;
};
type Http2EventListener = (...args: unknown[]) => void;

type SerializedHttp2SocketState = {
  encrypted?: boolean;
  allowHalfOpen?: boolean;
  localAddress?: string;
  localPort?: number;
  localFamily?: string;
  remoteAddress?: string;
  remotePort?: number;
  remoteFamily?: string;
  servername?: string;
  alpnProtocol?: string | false;
};

type SerializedHttp2SessionState = {
  encrypted?: boolean;
  alpnProtocol?: string | false;
  originSet?: string[];
  localSettings?: Http2SettingsRecord;
  remoteSettings?: Http2SettingsRecord;
  state?: Http2SessionRuntimeState;
  socket?: SerializedHttp2SocketState;
};

const http2Servers = new Map<number, Http2Server>();
const http2Sessions = new Map<number, Http2Session>();
const http2Streams = new Map<number, ClientHttp2Stream | ServerHttp2Stream>();
const pendingHttp2ClientStreamEvents = new Map<number, Array<{
  kind: "push" | "responseHeaders" | "data" | "end" | "close" | "error";
  data?: string;
  extraNumber?: number;
}>>();
const scheduledHttp2ClientStreamFlushes = new Set<number>();
const queuedHttp2DispatchEvents: Array<{
  kind: string;
  id: number;
  data?: string;
  extra?: string;
  extraNumber?: string | number;
  extraHeaders?: string;
  flags?: string | number;
}> = [];
const pendingHttp2CompatRequests = new Map<number, {
  serverId: number;
  requestJson: string;
}>();
let scheduledHttp2DispatchDrain = false;
let nextHttp2ServerId = 1;

class Http2EventEmitter {
  private _listeners: Record<string, Http2EventListener[]> = {};
  private _onceListeners: Record<string, Http2EventListener[]> = {};
  on(event: string, listener: Http2EventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  addListener(event: string, listener: Http2EventListener): this {
    return this.on(event, listener);
  }
  once(event: string, listener: Http2EventListener): this {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    return this;
  }
  removeListener(event: string, listener: Http2EventListener): this {
    const remove = (target?: Http2EventListener[]) => {
      if (!target) return;
      const index = target.indexOf(listener);
      if (index !== -1) target.splice(index, 1);
    };
    remove(this._listeners[event]);
    remove(this._onceListeners[event]);
    return this;
  }
  off(event: string, listener: Http2EventListener): this {
    return this.removeListener(event, listener);
  }
  listenerCount(event: string): number {
    return (this._listeners[event]?.length ?? 0) + (this._onceListeners[event]?.length ?? 0);
  }
  setMaxListeners(_value: number): this {
    return this;
  }
  emit(event: string, ...args: unknown[]): boolean {
    let handled = false;
    const listeners = this._listeners[event];
    if (listeners) {
      for (const listener of [...listeners]) {
        listener(...args);
        handled = true;
      }
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      this._onceListeners[event] = [];
      for (const listener of [...onceListeners]) {
        listener(...args);
        handled = true;
      }
    }
    return handled;
  }
}

class Http2SocketProxy extends Http2EventEmitter {
  allowHalfOpen = false;
  encrypted = false;
  localAddress = "127.0.0.1";
  localPort = 0;
  localFamily = "IPv4";
  remoteAddress = "127.0.0.1";
  remotePort = 0;
  remoteFamily = "IPv4";
  servername?: string;
  alpnProtocol: string | false = false;
  readable = true;
  writable = true;
  destroyed = false;
  _bridgeReadPollTimer: ReturnType<typeof setTimeout> | null = null;
  _loopbackServer: null = null;
  private _onDestroy?: () => void;
  private _destroyCallbackInvoked = false;
  constructor(
    state?: SerializedHttp2SocketState,
    onDestroy?: () => void,
  ) {
    super();
    this._onDestroy = onDestroy;
    this._applyState(state);
  }
  _applyState(state?: SerializedHttp2SocketState): void {
    if (!state) return;
    this.allowHalfOpen = state.allowHalfOpen === true;
    this.encrypted = state.encrypted === true;
    this.localAddress = state.localAddress ?? this.localAddress;
    this.localPort = state.localPort ?? this.localPort;
    this.localFamily = state.localFamily ?? this.localFamily;
    this.remoteAddress = state.remoteAddress ?? this.remoteAddress;
    this.remotePort = state.remotePort ?? this.remotePort;
    this.remoteFamily = state.remoteFamily ?? this.remoteFamily;
    this.servername = state.servername;
    this.alpnProtocol = state.alpnProtocol ?? this.alpnProtocol;
  }
  _clearTimeoutTimer(): void {
    // Borrowed net.Socket destroy paths call into this hook.
  }
  _emitNet(event: string, error?: Error): void {
    if (event === "error" && error) {
      this.emit("error", error);
      return;
    }
    if (event === "close") {
      if (!this._destroyCallbackInvoked) {
        this._destroyCallbackInvoked = true;
        queueMicrotask(() => {
          this._onDestroy?.();
        });
      }
      this.emit("close");
    }
  }
  end(): this {
    this.destroyed = true;
    this.readable = false;
    this.writable = false;
    this.emit("close");
    return this;
  }
  destroy(): this {
    if (this.destroyed) {
      return this;
    }
    this.destroyed = true;
    this.readable = false;
    this.writable = false;
    this._emitNet("close");
    return this;
  }
}

function createHttp2ArgTypeError(argumentName: string, expected: string, value: unknown): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The "${argumentName}" argument must be of type ${expected}. Received ${formatReceivedType(value)}`,
    "ERR_INVALID_ARG_TYPE",
  );
}

function createHttp2Error(code: string, message: string): Error & { code: string } {
  return createErrorWithCode(message, code);
}

function createHttp2SettingRangeError(setting: string, value: unknown): RangeError & { code: string } {
  const error = new RangeError(
    `Invalid value for setting "${setting}": ${String(value)}`,
  ) as RangeError & { code: string };
  error.code = "ERR_HTTP2_INVALID_SETTING_VALUE";
  return error;
}

function createHttp2SettingTypeError(setting: string, value: unknown): TypeError & { code: string } {
  const error = new TypeError(
    `Invalid value for setting "${setting}": ${String(value)}`,
  ) as TypeError & { code: string };
  error.code = "ERR_HTTP2_INVALID_SETTING_VALUE";
  return error;
}

const HTTP2_INTERNAL_BINDING_CONSTANTS = {
  NGHTTP2_NO_ERROR: 0,
  NGHTTP2_PROTOCOL_ERROR: 1,
  NGHTTP2_INTERNAL_ERROR: 2,
  NGHTTP2_FLOW_CONTROL_ERROR: 3,
  NGHTTP2_SETTINGS_TIMEOUT: 4,
  NGHTTP2_STREAM_CLOSED: 5,
  NGHTTP2_FRAME_SIZE_ERROR: 6,
  NGHTTP2_REFUSED_STREAM: 7,
  NGHTTP2_CANCEL: 8,
  NGHTTP2_COMPRESSION_ERROR: 9,
  NGHTTP2_CONNECT_ERROR: 10,
  NGHTTP2_ENHANCE_YOUR_CALM: 11,
  NGHTTP2_INADEQUATE_SECURITY: 12,
  NGHTTP2_HTTP_1_1_REQUIRED: 13,
  NGHTTP2_NV_FLAG_NONE: 0,
  NGHTTP2_NV_FLAG_NO_INDEX: 1,
  NGHTTP2_ERR_DEFERRED: -508,
  NGHTTP2_ERR_STREAM_ID_NOT_AVAILABLE: -509,
  NGHTTP2_ERR_STREAM_CLOSED: -510,
  NGHTTP2_ERR_INVALID_ARGUMENT: -501,
  NGHTTP2_ERR_FRAME_SIZE_ERROR: -522,
  NGHTTP2_ERR_NOMEM: -901,
  NGHTTP2_FLAG_NONE: 0,
  NGHTTP2_FLAG_END_STREAM: 1,
  NGHTTP2_FLAG_END_HEADERS: 4,
  NGHTTP2_FLAG_ACK: 1,
  NGHTTP2_FLAG_PADDED: 8,
  NGHTTP2_FLAG_PRIORITY: 32,
  NGHTTP2_DEFAULT_WEIGHT: 16,
  NGHTTP2_SETTINGS_HEADER_TABLE_SIZE: 1,
  NGHTTP2_SETTINGS_ENABLE_PUSH: 2,
  NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS: 3,
  NGHTTP2_SETTINGS_INITIAL_WINDOW_SIZE: 4,
  NGHTTP2_SETTINGS_MAX_FRAME_SIZE: 5,
  NGHTTP2_SETTINGS_MAX_HEADER_LIST_SIZE: 6,
  NGHTTP2_SETTINGS_ENABLE_CONNECT_PROTOCOL: 8,
} as const;

const HTTP2_NGHTTP2_ERROR_MESSAGES: Record<number, string> = {
  [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_DEFERRED]: "Data deferred",
  [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_STREAM_ID_NOT_AVAILABLE]: "Stream ID is not available",
  [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_STREAM_CLOSED]: "Stream was already closed or invalid",
  [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_INVALID_ARGUMENT]: "Invalid argument",
  [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_FRAME_SIZE_ERROR]: "Frame size error",
  [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_NOMEM]: "Out of memory",
};

class NghttpError extends Error {
  code = "ERR_HTTP2_ERROR";

  constructor(message: string) {
    super(message);
    this.name = "Error";
  }
}

function nghttp2ErrorString(code: number): string {
  return HTTP2_NGHTTP2_ERROR_MESSAGES[code] ?? `HTTP/2 error (${String(code)})`;
}

function createHttp2InvalidArgValueError(property: string, value: unknown): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The property 'options.${property}' is invalid. Received ${formatHttp2InvalidValue(value)}`,
    "ERR_INVALID_ARG_VALUE",
  );
}

function formatHttp2InvalidValue(value: unknown): string {
  if (typeof value === "function") {
    return `[Function${value.name ? `: ${value.name}` : ": function"}]`;
  }
  if (typeof value === "symbol") {
    return value.toString();
  }
  if (Array.isArray(value)) {
    return "[]";
  }
  if (value === null) {
    return "null";
  }
  if (typeof value === "object") {
    return "{}";
  }
  return String(value);
}

function createHttp2PayloadForbiddenError(statusCode: number): Error & { code: string } {
  return createHttp2Error(
    "ERR_HTTP2_PAYLOAD_FORBIDDEN",
    `Responses with ${String(statusCode)} status must not have a payload`,
  );
}

type Http2BridgeStatPayload = {
  mode: number;
  size: number;
  atimeMs?: number;
  mtimeMs?: number;
  ctimeMs?: number;
  birthtimeMs?: number;
};

type Http2BridgeStat = {
  size: number;
  mode: number;
  atimeMs: number;
  mtimeMs: number;
  ctimeMs: number;
  birthtimeMs: number;
  atime: Date;
  mtime: Date;
  ctime: Date;
  birthtime: Date;
  isFile(): boolean;
  isDirectory(): boolean;
  isFIFO(): boolean;
  isSocket(): boolean;
  isSymbolicLink(): boolean;
};

type Http2FileResponseOptions = {
  offset: number;
  length: number | undefined;
  statCheck?: (stat: Http2BridgeStat, headers: Record<string, unknown>, options: { offset: number; length: number }) => void;
  onError?: (error: Error) => void;
};

const S_IFMT = 0o170000;
const S_IFDIR = 0o040000;
const S_IFREG = 0o100000;
const S_IFIFO = 0o010000;
const S_IFSOCK = 0o140000;
const S_IFLNK = 0o120000;

function createHttp2BridgeStat(stat: Http2BridgeStatPayload): Http2BridgeStat {
  const atimeMs = stat.atimeMs ?? 0;
  const mtimeMs = stat.mtimeMs ?? atimeMs;
  const ctimeMs = stat.ctimeMs ?? mtimeMs;
  const birthtimeMs = stat.birthtimeMs ?? ctimeMs;
  const fileType = stat.mode & S_IFMT;
  return {
    size: stat.size,
    mode: stat.mode,
    atimeMs,
    mtimeMs,
    ctimeMs,
    birthtimeMs,
    atime: new Date(atimeMs),
    mtime: new Date(mtimeMs),
    ctime: new Date(ctimeMs),
    birthtime: new Date(birthtimeMs),
    isFile: () => fileType === S_IFREG,
    isDirectory: () => fileType === S_IFDIR,
    isFIFO: () => fileType === S_IFIFO,
    isSocket: () => fileType === S_IFSOCK,
    isSymbolicLink: () => fileType === S_IFLNK,
  };
}

function normalizeHttp2FileResponseOptions(options?: Record<string, unknown>): Http2FileResponseOptions {
  const normalized = options ?? {};
  const offset = normalized.offset;
  if (offset !== undefined && (typeof offset !== "number" || !Number.isFinite(offset))) {
    throw createHttp2InvalidArgValueError("offset", offset);
  }
  const length = normalized.length;
  if (length !== undefined && (typeof length !== "number" || !Number.isFinite(length))) {
    throw createHttp2InvalidArgValueError("length", length);
  }
  const statCheck = normalized.statCheck;
  if (statCheck !== undefined && typeof statCheck !== "function") {
    throw createHttp2InvalidArgValueError("statCheck", statCheck);
  }
  const onError = normalized.onError;
  return {
    offset: offset === undefined ? 0 : Math.max(0, Math.trunc(offset)),
    length:
      typeof length === "number"
        ? Math.trunc(length)
        : undefined,
    statCheck: typeof statCheck === "function" ? statCheck as Http2FileResponseOptions["statCheck"] : undefined,
    onError: typeof onError === "function" ? onError as Http2FileResponseOptions["onError"] : undefined,
  };
}

function sliceHttp2FileBody(body: Buffer, offset: number, length: number | undefined): Buffer {
  const safeOffset = Math.max(0, Math.min(offset, body.length));
  if (length === undefined || length < 0) {
    return body.subarray(safeOffset);
  }
  return body.subarray(safeOffset, Math.min(body.length, safeOffset + length));
}

class Http2Stream {
  constructor(private readonly _streamId: number) {}

  respond(headers?: Http2HeadersRecord): number {
    if (typeof _networkHttp2StreamRespondRaw === "undefined") {
      throw new Error("http2 server stream respond bridge is not available");
    }
    _networkHttp2StreamRespondRaw.applySync(undefined, [
      this._streamId,
      serializeHttp2Headers(headers),
    ]);
    return 0;
  }
}

const DEFAULT_HTTP2_SETTINGS: Http2SettingsRecord = {
  headerTableSize: 4096,
  enablePush: true,
  initialWindowSize: 65535,
  maxFrameSize: 16384,
  maxConcurrentStreams: 4294967295,
  maxHeaderListSize: 65535,
  maxHeaderSize: 65535,
  enableConnectProtocol: false,
};

const DEFAULT_HTTP2_SESSION_STATE: Http2SessionRuntimeState = {
  effectiveLocalWindowSize: 65535,
  localWindowSize: 65535,
  remoteWindowSize: 65535,
  nextStreamID: 1,
  outboundQueueSize: 1,
  deflateDynamicTableSize: 0,
  inflateDynamicTableSize: 0,
};

function cloneHttp2Settings(settings?: Http2SettingsRecord | null): Http2SettingsRecord {
  const cloned: Http2SettingsRecord = {};
  for (const [key, value] of Object.entries(settings ?? {})) {
    if (key === "customSettings" && value && typeof value === "object") {
      const customSettings: Record<number, number> = {};
      for (const [customKey, customValue] of Object.entries(value as Record<string, number>)) {
        customSettings[Number(customKey)] = Number(customValue);
      }
      cloned.customSettings = customSettings;
      continue;
    }
    cloned[key] = value as boolean | number;
  }
  return cloned;
}

function cloneHttp2SessionRuntimeState(
  state?: Http2SessionRuntimeState | null,
): Http2SessionRuntimeState {
  return {
    ...DEFAULT_HTTP2_SESSION_STATE,
    ...(state ?? {}),
  };
}

function parseHttp2SessionRuntimeState(
  state?: unknown,
): Http2SessionRuntimeState | undefined {
  if (!state || typeof state !== "object") {
    return undefined;
  }
  const record = state as Record<string, unknown>;
  const parsed: Http2SessionRuntimeState = {};
  const numericKeys = [
    "effectiveLocalWindowSize",
    "localWindowSize",
    "remoteWindowSize",
    "nextStreamID",
    "outboundQueueSize",
    "deflateDynamicTableSize",
    "inflateDynamicTableSize",
  ] as const;
  for (const key of numericKeys) {
    if (typeof record[key] === "number") {
      parsed[key] = record[key] as number;
    }
  }
  return parsed;
}

function validateHttp2Settings(settings: unknown, argumentName = "settings"): Http2SettingsRecord {
  if (!settings || typeof settings !== "object" || Array.isArray(settings)) {
    throw createHttp2ArgTypeError(argumentName, "object", settings);
  }
  const record = settings as Record<string, unknown>;
  const normalized: Http2SettingsRecord = {};
  const numberRanges: Record<string, [number, number]> = {
    headerTableSize: [0, 4294967295],
    initialWindowSize: [0, 4294967295],
    maxFrameSize: [16384, 16777215],
    maxConcurrentStreams: [0, 4294967295],
    maxHeaderListSize: [0, 4294967295],
    maxHeaderSize: [0, 4294967295],
  };
  for (const [key, value] of Object.entries(record)) {
    if (value === undefined) {
      continue;
    }
    if (key === "enablePush" || key === "enableConnectProtocol") {
      if (typeof value !== "boolean") {
        throw createHttp2SettingTypeError(key, value);
      }
      normalized[key] = value;
      continue;
    }
    if (key === "customSettings") {
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw createHttp2SettingRangeError(key, value);
      }
      const customSettings: Record<number, number> = {};
      for (const [customKey, customValue] of Object.entries(value as Record<string, unknown>)) {
        const numericKey = Number(customKey);
        if (!Number.isInteger(numericKey) || numericKey < 0 || numericKey > 0xffff) {
          throw createHttp2SettingRangeError(key, value);
        }
        if (
          typeof customValue !== "number" ||
          !Number.isInteger(customValue) ||
          customValue < 0 ||
          customValue > 4294967295
        ) {
          throw createHttp2SettingRangeError(key, value);
        }
        customSettings[numericKey] = customValue;
      }
      normalized.customSettings = customSettings;
      continue;
    }
    if (key in numberRanges) {
      const [min, max] = numberRanges[key]!;
      if (
        typeof value !== "number" ||
        !Number.isInteger(value) ||
        value < min ||
        value > max
      ) {
        throw createHttp2SettingRangeError(key, value);
      }
      normalized[key] = value;
      continue;
    }
    normalized[key] = value as boolean | number | Record<number, number>;
  }
  return normalized;
}

function serializeHttp2Headers(headers?: Http2HeadersRecord): string {
  return JSON.stringify(headers ?? {});
}

function parseHttp2Headers(headersJson?: string): Http2HeadersRecord {
  if (!headersJson) {
    return {};
  }
  try {
    const parsed = JSON.parse(headersJson) as Http2HeadersRecord;
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function parseHttp2SessionState(data?: string): SerializedHttp2SessionState | null {
  if (!data) {
    return null;
  }
  try {
    const parsed = JSON.parse(data) as SerializedHttp2SessionState;
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

function parseHttp2SocketState(data?: string): SerializedHttp2SocketState | null {
  if (!data) {
    return null;
  }
  try {
    const parsed = JSON.parse(data) as SerializedHttp2SocketState;
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

function parseHttp2ErrorPayload(data?: string): Error {
  if (!data) {
    return new Error("Unknown HTTP/2 bridge error");
  }
  try {
    const parsed = JSON.parse(data) as { message?: string; name?: string; code?: string };
    const error = new Error(parsed.message ?? "Unknown HTTP/2 bridge error") as Error & { code?: string };
    if (parsed.name) error.name = parsed.name;
    if (parsed.code) error.code = parsed.code;
    return error;
  } catch {
    return new Error(data);
  }
}

function normalizeHttp2Headers(headers?: Http2HeadersRecord): Http2HeadersRecord {
  const normalized: Http2HeadersRecord = {};
  if (!headers || typeof headers !== "object") {
    return normalized;
  }
  for (const [key, value] of Object.entries(headers)) {
    normalized[String(key)] = value;
  }
  return normalized;
}

function validateHttp2RequestOptions(options?: Record<string, unknown>): void {
  if (!options) {
    return;
  }
  const validators: Record<string, string> = {
    endStream: "boolean",
    weight: "number",
    parent: "number",
    exclusive: "boolean",
    silent: "boolean",
  };
  for (const [key, expectedType] of Object.entries(validators)) {
    if (!(key in options) || options[key] === undefined) {
      continue;
    }
    const value = options[key];
    if (expectedType === "boolean" && typeof value !== "boolean") {
      throw createHttp2ArgTypeError(key, "boolean", value);
    }
    if (expectedType === "number" && typeof value !== "number") {
      throw createHttp2ArgTypeError(key, "number", value);
    }
  }
}

function validateHttp2ConnectOptions(options?: Record<string, unknown>): void {
  if (!options || !options.settings || typeof options.settings !== "object") {
    return;
  }
  const settings = options.settings as Record<string, unknown>;
  if ("maxFrameSize" in settings) {
    const value = settings.maxFrameSize;
    if (typeof value !== "number" || !Number.isInteger(value) || value < 16384 || value > 16777215) {
      throw createHttp2SettingRangeError("maxFrameSize", value);
    }
  }
}

function applyHttp2SessionState(
  session: Http2Session,
  state?: SerializedHttp2SessionState | null,
): void {
  if (!state) {
    return;
  }
  session.encrypted = state.encrypted === true;
  session.alpnProtocol = state.alpnProtocol ?? (session.encrypted ? "h2" : "h2c");
  session.originSet = Array.isArray(state.originSet) && state.originSet.length > 0
    ? [...state.originSet]
    : session.encrypted
      ? []
      : undefined;
  if (state.localSettings && typeof state.localSettings === "object") {
    session.localSettings = cloneHttp2Settings(state.localSettings);
  }
  if (state.remoteSettings && typeof state.remoteSettings === "object") {
    session.remoteSettings = cloneHttp2Settings(state.remoteSettings);
  }
  if (state.state && typeof state.state === "object") {
    session._applyRuntimeState(parseHttp2SessionRuntimeState(state.state));
  }
  session.socket._applyState(state.socket);
}

function normalizeHttp2Authority(
  authority: unknown,
  options?: Record<string, unknown>,
): URL {
  if (authority instanceof URL) {
    return authority;
  }
  if (typeof authority === "string") {
    return new URL(authority);
  }
  if (authority && typeof authority === "object") {
    const record = authority as Record<string, unknown>;
    const protocol =
      typeof (options?.protocol ?? record.protocol) === "string"
        ? String(options?.protocol ?? record.protocol)
        : "http:";
    const hostname =
      typeof (options?.host ?? record.host ?? options?.hostname ?? record.hostname) === "string"
        ? String(options?.host ?? record.host ?? options?.hostname ?? record.hostname)
        : "localhost";
    const portValue = options?.port ?? record.port;
    const port = portValue === undefined ? "" : String(portValue);
    return new URL(`${protocol}//${hostname}${port ? `:${port}` : ""}`);
  }
  return new URL("http://localhost");
}

function normalizeHttp2ConnectArgs(
  authorityOrOptions: unknown,
  optionsOrListener?: Record<string, unknown> | ((session: Http2Session) => void),
  maybeListener?: (session: Http2Session) => void,
): {
  authority: URL;
  options: Record<string, unknown>;
  listener?: (session: Http2Session) => void;
} {
  const listener =
    typeof optionsOrListener === "function"
      ? optionsOrListener
      : typeof maybeListener === "function"
        ? maybeListener
        : undefined;
  const options =
    typeof optionsOrListener === "function"
      ? {}
      : (optionsOrListener ?? {});
  return {
    authority: normalizeHttp2Authority(authorityOrOptions, options),
    options,
    listener,
  };
}

function resolveHttp2SocketId(socket: unknown): number | undefined {
  if (!socket || typeof socket !== "object") {
    return undefined;
  }
  const value = (socket as { _socketId?: unknown })._socketId;
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

class ClientHttp2Stream extends Http2EventEmitter {
  private _streamId: number;
  private _encoding?: BufferEncoding;
  private _utf8Remainder?: Buffer;
  private _isPushStream: boolean;
  private _session?: Http2Session;
  private _receivedResponse = false;
  private _needsDrain = false;
  private _pendingWritableBytes = 0;
  private _drainScheduled = false;
  private readonly _writableHighWaterMark = 16 * 1024;
  rstCode = 0;
  readable = true;
  writable = true;
  writableEnded = false;
  writableFinished = false;
  destroyed = false;
  _writableState = { ended: false, finished: false, objectMode: false, corked: 0, length: 0 };
  constructor(streamId: number, session?: Http2Session, isPushStream = false) {
    super();
    this._streamId = streamId;
    this._session = session;
    this._isPushStream = isPushStream;
    if (!isPushStream) {
      queueMicrotask(() => {
        this.emit("ready");
      });
    }
  }
  setEncoding(encoding: string): this {
    this._encoding = encoding as BufferEncoding;
    this._utf8Remainder =
      this._encoding === "utf8" || this._encoding === "utf-8"
        ? Buffer.alloc(0)
        : undefined;
    return this;
  }
  close(): this {
    this.end();
    return this;
  }
  destroy(error?: Error): this {
    if (this.destroyed) {
      return this;
    }
    this.destroyed = true;
    if (error) {
      this.emit("error", error);
    }
    this.end();
    return this;
  }
  private _scheduleDrain(): void {
    if (!this._needsDrain || this._drainScheduled) {
      return;
    }
    this._drainScheduled = true;
    queueMicrotask(() => {
      this._drainScheduled = false;
      if (!this._needsDrain) {
        return;
      }
      this._needsDrain = false;
      this._pendingWritableBytes = 0;
      this.emit("drain");
    });
  }
  write(data: unknown, encodingOrCallback?: BufferEncoding | (() => void), callback?: () => void): boolean {
    if (typeof _networkHttp2StreamWriteRaw === "undefined") {
      throw new Error("http2 session stream write bridge is not available");
    }
    const buffer = Buffer.isBuffer(data)
      ? data
      : typeof data === "string"
        ? Buffer.from(data, typeof encodingOrCallback === "string" ? encodingOrCallback : "utf8")
        : Buffer.from(data as Uint8Array);
    const wrote = _networkHttp2StreamWriteRaw.applySync(undefined, [this._streamId, buffer.toString("base64")]);
    this._pendingWritableBytes += buffer.byteLength;
    const shouldBackpressure = wrote === false || this._pendingWritableBytes >= this._writableHighWaterMark;
    if (shouldBackpressure) {
      this._needsDrain = true;
    }
    const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    cb?.();
    return !shouldBackpressure;
  }
  end(data?: unknown): this {
    if (typeof _networkHttp2StreamEndRaw === "undefined") {
      throw new Error("http2 session stream end bridge is not available");
    }
    let encoded: string | null = null;
    if (data !== undefined) {
      const buffer = Buffer.isBuffer(data)
        ? data
        : typeof data === "string"
          ? Buffer.from(data)
          : Buffer.from(data as Uint8Array);
      encoded = buffer.toString("base64");
    }
    _networkHttp2StreamEndRaw.applySync(undefined, [this._streamId, encoded]);
    this.writableEnded = true;
    this._writableState.ended = true;
    queueMicrotask(() => {
      this.writable = false;
      this.writableFinished = true;
      this._writableState.finished = true;
      this.emit("finish");
    });
    return this;
  }
  resume(): this {
    return this;
  }
  _emitPush(headers: Http2HeadersRecord, flags?: number): void {
    if (process.env.SECURE_EXEC_DEBUG_HTTP2_BRIDGE === "1") {
      console.error("[secure-exec http2 isolate] push", this._streamId);
    }
    this.emit("push", headers, flags ?? 0);
  }
  _hasReceivedResponse(): boolean {
    return this._receivedResponse;
  }
  _belongsTo(session: Http2Session): boolean {
    return this._session === session;
  }
  _emitResponseHeaders(headers: Http2HeadersRecord): void {
    this._receivedResponse = true;
    if (process.env.SECURE_EXEC_DEBUG_HTTP2_BRIDGE === "1") {
      console.error("[secure-exec http2 isolate] response headers", this._streamId, this._isPushStream);
    }
    if (!this._isPushStream) {
      this.emit("response", headers);
    }
  }
  _emitDataChunk(dataBase64?: string): void {
    if (!dataBase64) {
      return;
    }
    const chunkBuffer = Buffer.from(dataBase64, "base64");
    if (this._utf8Remainder !== undefined) {
      const buffer =
        this._utf8Remainder.length > 0
          ? Buffer.concat([this._utf8Remainder, chunkBuffer])
          : chunkBuffer;
      const completeLength = getCompleteUtf8PrefixLength(buffer);
      const chunk = buffer.subarray(0, completeLength).toString("utf8");
      this._utf8Remainder =
        completeLength < buffer.length ? buffer.subarray(completeLength) : Buffer.alloc(0);
      if (chunk.length > 0) {
        this.emit("data", chunk);
      }
    } else if (this._encoding) {
      this.emit("data", chunkBuffer.toString(this._encoding));
    } else {
      this.emit("data", chunkBuffer);
    }
    this._scheduleDrain();
  }
  _emitEnd(): void {
    if (this._utf8Remainder && this._utf8Remainder.length > 0) {
      const trailing = this._utf8Remainder.toString("utf8");
      this._utf8Remainder = Buffer.alloc(0);
      if (trailing.length > 0) {
        this.emit("data", trailing);
      }
    }
    this.readable = false;
    this.emit("end");
    this._scheduleDrain();
  }
  _emitClose(rstCode?: number): void {
    if (typeof rstCode === "number") {
      this.rstCode = rstCode;
    }
    this.destroyed = true;
    this.readable = false;
    this.writable = false;
    this._scheduleDrain();
    this.emit("close");
  }
}

function getCompleteUtf8PrefixLength(buffer: Buffer): number {
  if (buffer.length === 0) {
    return 0;
  }
  let continuationCount = 0;
  for (let index = buffer.length - 1; index >= 0 && continuationCount < 3; index -= 1) {
    if ((buffer[index] & 0xc0) !== 0x80) {
      const trailingBytes = buffer.length - index;
      const lead = buffer[index];
      const expectedBytes =
        (lead & 0x80) === 0
          ? 1
          : (lead & 0xe0) === 0xc0
            ? 2
            : (lead & 0xf0) === 0xe0
              ? 3
              : (lead & 0xf8) === 0xf0
                ? 4
                : 1;
      return trailingBytes < expectedBytes ? index : buffer.length;
    }
    continuationCount += 1;
  }
  return continuationCount > 0 ? buffer.length - continuationCount : buffer.length;
}

class ServerHttp2Stream extends Http2EventEmitter {
  private _streamId: number;
  private _binding: Http2Stream;
  private _responded = false;
  private _endQueued = false;
  private _pendingSyntheticErrorSuppressions = 0;
  private _requestHeaders?: Http2HeadersRecord;
  private _isPushStream: boolean;
  session: Http2Session;
  rstCode = 0;
  readable = true;
  writable = true;
  destroyed = false;
  _readableState: {
    flowing: boolean | null;
    ended: boolean;
    highWaterMark: number;
  };
  _writableState: { ended: boolean };
  constructor(
    streamId: number,
    session: Http2Session,
    requestHeaders?: Http2HeadersRecord,
    isPushStream = false,
  ) {
    super();
    this._streamId = streamId;
    this._binding = new Http2Stream(streamId);
    this.session = session;
    this._requestHeaders = requestHeaders;
    this._isPushStream = isPushStream;
    this._readableState = {
      flowing: null,
      ended: false,
      highWaterMark: 16 * 1024,
    };
    this._writableState = {
      ended: requestHeaders?.[":method"] === "HEAD",
    };
  }
  private _closeWithCode(code: number): void {
    this.rstCode = code;
    _networkHttp2StreamCloseRaw?.applySync(undefined, [this._streamId, code]);
  }
  private _markSyntheticClose(): void {
    this.destroyed = true;
    this.readable = false;
    this.writable = false;
  }
  _shouldSuppressHostError(): boolean {
    if (this._pendingSyntheticErrorSuppressions <= 0) {
      return false;
    }
    this._pendingSyntheticErrorSuppressions -= 1;
    return true;
  }
  private _emitNghttp2Error(errorCode: number): void {
    const error = new NghttpError(nghttp2ErrorString(errorCode));
    this._pendingSyntheticErrorSuppressions += 1;
    this._markSyntheticClose();
    this.emit("error", error);
    this._closeWithCode(HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_INTERNAL_ERROR);
  }
  private _emitInternalStreamError(): void {
    const error = createHttp2Error(
      "ERR_HTTP2_STREAM_ERROR",
      "Stream closed with error code NGHTTP2_INTERNAL_ERROR",
    );
    this._pendingSyntheticErrorSuppressions += 1;
    this._markSyntheticClose();
    this.emit("error", error);
    this._closeWithCode(HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_INTERNAL_ERROR);
  }
  private _submitResponse(headers?: Http2HeadersRecord): boolean {
    this._responded = true;
    const ngError = this._binding.respond(headers);
    if (typeof ngError === "number" && ngError !== 0) {
      this._emitNghttp2Error(ngError);
      return false;
    }
    return true;
  }
  respond(headers?: Http2HeadersRecord): void {
    if (this.destroyed) {
      throw createHttp2Error("ERR_HTTP2_INVALID_STREAM", "The stream has been destroyed");
    }
    if (this._responded) {
      throw createHttp2Error("ERR_HTTP2_HEADERS_SENT", "Response has already been initiated.");
    }
    this._submitResponse(headers);
  }
  pushStream(
    headers: Http2HeadersRecord,
    optionsOrCallback?: Record<string, unknown> | ((error: Error | null, stream?: ServerHttp2Stream, headers?: Http2HeadersRecord) => void),
    maybeCallback?: (error: Error | null, stream?: ServerHttp2Stream, headers?: Http2HeadersRecord) => void,
  ): void {
    if (this._isPushStream) {
      throw createHttp2Error(
        "ERR_HTTP2_NESTED_PUSH",
        "A push stream cannot initiate another push stream.",
      );
    }
    const callback =
      typeof optionsOrCallback === "function"
        ? optionsOrCallback
        : maybeCallback;
    if (typeof callback !== "function") {
      throw createHttp2ArgTypeError("callback", "function", callback);
    }
    if (typeof _networkHttp2StreamPushStreamRaw === "undefined") {
      throw new Error("http2 server stream push bridge is not available");
    }
    const options =
      optionsOrCallback && typeof optionsOrCallback === "object" && !Array.isArray(optionsOrCallback)
        ? optionsOrCallback
        : {};
    const resultJson = _networkHttp2StreamPushStreamRaw.applySync(
      undefined,
      [
        this._streamId,
        serializeHttp2Headers(normalizeHttp2Headers(headers)),
        JSON.stringify(options ?? {}),
      ],
    );
    const result = JSON.parse(resultJson) as {
      error?: string;
      streamId?: number;
      headers?: string;
    };
    if (result.error) {
      callback(parseHttp2ErrorPayload(result.error));
      return;
    }
    const pushStream = new ServerHttp2Stream(
      Number(result.streamId),
      this.session,
      parseHttp2Headers(result.headers),
      true,
    );
    http2Streams.set(Number(result.streamId), pushStream);
    callback(null, pushStream, parseHttp2Headers(result.headers));
  }
  write(data: unknown): boolean {
    if (this._writableState.ended) {
      queueMicrotask(() => {
        this.emit("error", createHttp2Error("ERR_STREAM_WRITE_AFTER_END", "write after end"));
      });
      return false;
    }
    if (typeof _networkHttp2StreamWriteRaw === "undefined") {
      throw new Error("http2 server stream write bridge is not available");
    }
    const buffer = Buffer.isBuffer(data)
      ? data
      : typeof data === "string"
        ? Buffer.from(data)
        : Buffer.from(data as Uint8Array);
    return _networkHttp2StreamWriteRaw.applySync(undefined, [this._streamId, buffer.toString("base64")]);
  }
  end(data?: unknown): void {
    if (!this._responded) {
      if (!this._submitResponse({ ":status": 200 })) {
        return;
      }
    }
    if (this._endQueued) {
      return;
    }
    if (typeof _networkHttp2StreamEndRaw === "undefined") {
      throw new Error("http2 server stream end bridge is not available");
    }
    this._writableState.ended = true;
    let encoded: string | null = null;
    if (data !== undefined) {
      const buffer = Buffer.isBuffer(data)
        ? data
        : typeof data === "string"
        ? Buffer.from(data)
        : Buffer.from(data as Uint8Array);
      encoded = buffer.toString("base64");
    }
    this._endQueued = true;
    queueMicrotask(() => {
      if (!this._endQueued || this.destroyed) {
        return;
      }
      this._endQueued = false;
      _networkHttp2StreamEndRaw.applySync(undefined, [this._streamId, encoded]);
    });
  }
  pause(): this {
    this._readableState.flowing = false;
    _networkHttp2StreamPauseRaw?.applySync(undefined, [this._streamId]);
    return this;
  }
  resume(): this {
    this._readableState.flowing = true;
    _networkHttp2StreamResumeRaw?.applySync(undefined, [this._streamId]);
    return this;
  }
  respondWithFile(
    path: string,
    headers?: Record<string, unknown>,
    options?: Record<string, unknown>
  ): void {
    if (this.destroyed) {
      throw createHttp2Error("ERR_HTTP2_INVALID_STREAM", "The stream has been destroyed");
    }
    if (this._responded) {
      throw createHttp2Error("ERR_HTTP2_HEADERS_SENT", "Response has already been initiated.");
    }
    const normalizedOptions = normalizeHttp2FileResponseOptions(options);
    const responseHeaders = { ...(headers ?? {}) };
    const statusCode = responseHeaders[":status"];
    if (statusCode === 204 || statusCode === 205 || statusCode === 304) {
      throw createHttp2PayloadForbiddenError(Number(statusCode));
    }

    try {
      const statJson = _fs.stat.applySyncPromise(undefined, [path]);
      const bodyBase64 = _fs.readFileBinary.applySyncPromise(undefined, [path]);
      const stat = createHttp2BridgeStat(JSON.parse(statJson) as Http2BridgeStatPayload);
      const callbackOptions = {
        offset: normalizedOptions.offset,
        length: normalizedOptions.length ?? Math.max(0, stat.size - normalizedOptions.offset),
      };
      normalizedOptions.statCheck?.(stat, responseHeaders, callbackOptions);
      const body = Buffer.from(bodyBase64, "base64");
      const slicedBody = sliceHttp2FileBody(
        body,
        normalizedOptions.offset,
        normalizedOptions.length,
      );
      if (responseHeaders["content-length"] === undefined) {
        responseHeaders["content-length"] = slicedBody.byteLength;
      }
      if (!this._submitResponse({
        ":status": 200,
        ...(responseHeaders as Http2HeadersRecord),
      })) {
        return;
      }
      this.end(slicedBody);
      return;
    } catch {
      // Fall back to the host http2 helper when the path is not available through the VFS bridge.
    }
    if (typeof _networkHttp2StreamRespondWithFileRaw === "undefined") {
      throw new Error("http2 server stream respondWithFile bridge is not available");
    }
    this._responded = true;
    _networkHttp2StreamRespondWithFileRaw.applySync(
      undefined,
      [
        this._streamId,
        path,
        JSON.stringify(headers ?? {}),
        JSON.stringify(options ?? {}),
      ],
    );
  }
  respondWithFD(
    fdOrHandle: number | { fd?: unknown },
    headers?: Record<string, unknown>,
    options?: Record<string, unknown>
  ): void {
    const fd =
      typeof fdOrHandle === "number"
        ? fdOrHandle
        : typeof fdOrHandle?.fd === "number"
          ? fdOrHandle.fd
          : NaN;
    const path = Number.isFinite(fd) ? _fdGetPath.applySync(undefined, [fd]) : null;
    if (!path) {
      this._emitInternalStreamError();
      return;
    }
    this.respondWithFile(path, headers, options);
  }
  destroy(error?: Error): this {
    if (this.destroyed) {
      return this;
    }
    this.destroyed = true;
    if (error) {
      this.emit("error", error);
    }
    this._closeWithCode(HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_CANCEL);
    return this;
  }
  _emitData(dataBase64?: string): void {
    if (!dataBase64) {
      return;
    }
    this.emit("data", Buffer.from(dataBase64, "base64"));
  }
  _emitEnd(): void {
    this._readableState.ended = true;
    this.emit("end");
  }
  _emitDrain(): void {
    this.emit("drain");
  }
  _emitClose(rstCode?: number): void {
    if (typeof rstCode === "number") {
      this.rstCode = rstCode;
    }
    this.destroyed = true;
    this.emit("close");
  }
}

class Http2ServerRequest extends Http2EventEmitter {
  headers: Http2HeadersRecord;
  method: string;
  url: string;
  connection: Http2SocketProxy;
  socket: Http2SocketProxy;
  stream: ServerHttp2Stream;
  destroyed = false;
  readable = true;
  _readableState = { flowing: null as boolean | null, length: 0, ended: false, objectMode: false };
  constructor(headers: Http2HeadersRecord, socket: Http2SocketProxy, stream: ServerHttp2Stream) {
    super();
    this.headers = headers;
    this.method = typeof headers[":method"] === "string" ? String(headers[":method"]) : "GET";
    this.url = typeof headers[":path"] === "string" ? String(headers[":path"]) : "/";
    this.connection = socket;
    this.socket = socket;
    this.stream = stream;
  }
  on(event: string, listener: Http2EventListener): this {
    super.on(event, listener);
    if (event === "data" && this._readableState.flowing !== false) {
      this.resume();
    }
    return this;
  }
  once(event: string, listener: Http2EventListener): this {
    super.once(event, listener);
    if (event === "data" && this._readableState.flowing !== false) {
      this.resume();
    }
    return this;
  }
  resume(): this {
    this._readableState.flowing = true;
    this.stream.resume();
    return this;
  }
  pause(): this {
    this._readableState.flowing = false;
    this.stream.pause();
    return this;
  }
  pipe(dest: {
    write: (chunk: Buffer) => boolean;
    end: () => void;
    once?: (event: string, listener: () => void) => unknown;
  }): typeof dest {
    this.on("data", (chunk) => {
      const wrote = dest.write(chunk as Buffer);
      if (wrote === false && typeof dest.once === "function") {
        this.pause();
        dest.once("drain", () => this.resume());
      }
    });
    this.on("end", () => dest.end());
    this.resume();
    return dest;
  }
  unpipe(): this { return this; }
  read(): null { return null; }
  isPaused(): boolean { return this._readableState.flowing === false; }
  setEncoding(): this { return this; }
  _emitData(chunk: Buffer): void {
    this._readableState.length += chunk.byteLength;
    this.emit("data", chunk);
  }
  _emitEnd(): void {
    this._readableState.ended = true;
    this.emit("end");
    this.emit("close");
  }
  _emitError(error: Error): void {
    this.emit("error", error);
  }
  destroy(err?: Error): this {
    this.destroyed = true;
    if (err) {
      this.emit("error", err);
    }
    this.emit("close");
    return this;
  }
}

class Http2ServerResponse extends Http2EventEmitter {
  private _stream: ServerHttp2Stream;
  private _headers: Http2HeadersRecord = {};
  private _statusCode = 200;
  headersSent = false;
  writable = true;
  writableEnded = false;
  writableFinished = false;
  socket: Http2SocketProxy;
  connection: Http2SocketProxy;
  stream: ServerHttp2Stream;
  _writableState = { ended: false, finished: false, objectMode: false, corked: 0, length: 0 };
  constructor(stream: ServerHttp2Stream) {
    super();
    this._stream = stream;
    this.stream = stream;
    this.socket = stream.session.socket;
    this.connection = this.socket;
  }
  writeHead(statusCode: number, headers?: Http2HeadersRecord): this {
    this._statusCode = statusCode;
    this._headers = {
      ...this._headers,
      ...(headers ?? {}),
      ":status": statusCode,
    };
    this._stream.respond(this._headers);
    this.headersSent = true;
    return this;
  }
  setHeader(name: string, value: Http2HeaderValue): this {
    this._headers[name] = value;
    return this;
  }
  getHeader(name: string): Http2HeaderValue | undefined {
    return this._headers[name];
  }
  hasHeader(name: string): boolean {
    return Object.prototype.hasOwnProperty.call(this._headers, name);
  }
  removeHeader(name: string): void {
    delete this._headers[name];
  }
  write(data: unknown, encodingOrCallback?: BufferEncoding | (() => void), callback?: () => void): boolean {
    if (!(":status" in this._headers)) {
      this._headers[":status"] = this._statusCode;
      this._stream.respond(this._headers);
      this.headersSent = true;
    }
    const wrote = this._stream.write(
      typeof data === "string" && typeof encodingOrCallback === "string"
        ? Buffer.from(data, encodingOrCallback)
        : data,
    );
    const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    cb?.();
    return wrote;
  }
  end(data?: unknown): this {
    if (!(":status" in this._headers)) {
      this._headers[":status"] = this._statusCode;
      this._stream.respond(this._headers);
      this.headersSent = true;
    }
    this.writableEnded = true;
    this._writableState.ended = true;
    this._stream.end(data);
    queueMicrotask(() => {
      this.writable = false;
      this.writableFinished = true;
      this._writableState.finished = true;
      this.emit("finish");
      this.emit("close");
    });
    return this;
  }
  destroy(err?: Error): this {
    if (err) {
      this.emit("error", err);
    }
    this.writable = false;
    this.writableEnded = true;
    this.writableFinished = true;
    this.emit("close");
    return this;
  }
}

class Http2Session extends Http2EventEmitter {
  encrypted = false;
  alpnProtocol: string | false = false;
  originSet?: string[];
  localSettings: Http2SettingsRecord = cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
  remoteSettings: Http2SettingsRecord = cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
  pendingSettingsAck = false;
  socket: Http2SocketProxy;
  state: Http2SessionRuntimeState = cloneHttp2SessionRuntimeState(DEFAULT_HTTP2_SESSION_STATE);
  private _sessionId: number;
  private _waitStarted = false;
  private _pendingSettingsAckCount = 0;
  private _awaitingInitialSettingsAck = false;
  private _settingsCallbacks: Array<() => void> = [];
  constructor(sessionId: number, socketState?: SerializedHttp2SocketState) {
    super();
    this._sessionId = sessionId;
    this.socket = new Http2SocketProxy(socketState, () => {
      setTimeout(() => {
        this.destroy();
      }, 0);
    });
    (this as Record<PropertyKey, unknown>)[HTTP2_K_SOCKET] = this.socket;
  }
  _retain(): void {
    if (this._waitStarted || typeof _networkHttp2SessionWaitRaw === "undefined") {
      return;
    }
    this._waitStarted = true;
    void _networkHttp2SessionWaitRaw.apply(undefined, [this._sessionId], {
      result: { promise: true },
    }).catch((error) => {
      this.emit("error", error instanceof Error ? error : new Error(String(error)));
    });
  }
  _release(): void {
    this._waitStarted = false;
  }
  _beginInitialSettingsAck(): void {
    this._awaitingInitialSettingsAck = true;
    this._pendingSettingsAckCount += 1;
    this.pendingSettingsAck = true;
  }
  _applyLocalSettings(settings: Http2SettingsRecord): void {
    this.localSettings = cloneHttp2Settings(settings);
    if (this._awaitingInitialSettingsAck) {
      this._awaitingInitialSettingsAck = false;
      this._pendingSettingsAckCount = Math.max(0, this._pendingSettingsAckCount - 1);
      this.pendingSettingsAck = this._pendingSettingsAckCount > 0;
    }
    this.emit("localSettings", this.localSettings);
  }
  _applyRemoteSettings(settings: Http2SettingsRecord): void {
    this.remoteSettings = cloneHttp2Settings(settings);
    this.emit("remoteSettings", this.remoteSettings);
  }
  _applyRuntimeState(state?: Http2SessionRuntimeState): void {
    this.state = cloneHttp2SessionRuntimeState(state);
  }
  _ackSettings(): void {
    this._pendingSettingsAckCount = Math.max(0, this._pendingSettingsAckCount - 1);
    this.pendingSettingsAck = this._pendingSettingsAckCount > 0;
    const callback = this._settingsCallbacks.shift();
    callback?.();
  }
  request(headers?: Http2HeadersRecord, options?: Record<string, unknown>): ClientHttp2Stream {
    if (typeof _networkHttp2SessionRequestRaw === "undefined") {
      throw new Error("http2 session request bridge is not available");
    }
    validateHttp2RequestOptions(options);
    const streamId = _networkHttp2SessionRequestRaw.applySync(
      undefined,
      [
        this._sessionId,
        serializeHttp2Headers(normalizeHttp2Headers(headers)),
        JSON.stringify(options ?? {}),
      ],
    );
    const stream = new ClientHttp2Stream(streamId, this);
    http2Streams.set(streamId, stream);
    return stream;
  }
  settings(settings: Record<string, unknown>, callback?: () => void): void {
    if (callback !== undefined && typeof callback !== "function") {
      throw createHttp2ArgTypeError("callback", "function", callback);
    }
    if (typeof _networkHttp2SessionSettingsRaw === "undefined") {
      throw new Error("http2 session settings bridge is not available");
    }
    const normalized = validateHttp2Settings(settings);
    _networkHttp2SessionSettingsRaw.applySync(
      undefined,
      [this._sessionId, JSON.stringify(normalized)],
    );
    this._pendingSettingsAckCount += 1;
    this.pendingSettingsAck = true;
    if (callback) {
      this._settingsCallbacks.push(callback);
    }
  }
  setLocalWindowSize(windowSize: unknown): void {
    if (typeof windowSize !== "number" || Number.isNaN(windowSize)) {
      throw createHttp2ArgTypeError("windowSize", "number", windowSize);
    }
    if (!Number.isInteger(windowSize) || windowSize < 0 || windowSize > 2147483647) {
      const error = new RangeError(
        `The value of "windowSize" is out of range. It must be >= 0 && <= 2147483647. Received ${windowSize}`,
      ) as RangeError & { code: string };
      error.code = "ERR_OUT_OF_RANGE";
      throw error;
    }
    if (typeof _networkHttp2SessionSetLocalWindowSizeRaw === "undefined") {
      throw new Error("http2 session setLocalWindowSize bridge is not available");
    }
    const result = _networkHttp2SessionSetLocalWindowSizeRaw.applySync(
      undefined,
      [this._sessionId, windowSize],
    );
    this._applyRuntimeState(parseHttp2SessionState(result)?.state);
  }
  goaway(code = 0, lastStreamID = 0, opaqueData?: unknown): void {
    const payload =
      opaqueData === undefined
        ? null
        : Buffer.isBuffer(opaqueData)
          ? opaqueData.toString("base64")
          : typeof opaqueData === "string"
            ? Buffer.from(opaqueData).toString("base64")
            : Buffer.from(opaqueData as Uint8Array).toString("base64");
    _networkHttp2SessionGoawayRaw?.applySync(undefined, [this._sessionId, code, lastStreamID, payload]);
  }
  close(): void {
    const pendingStreams = Array.from(http2Streams.entries()).filter(
      ([, stream]) =>
        typeof (stream as { _belongsTo?: unknown })._belongsTo === "function" &&
        (stream as ClientHttp2Stream)._belongsTo(this) &&
        !(stream as ClientHttp2Stream)._hasReceivedResponse(),
    ) as Array<[number, ClientHttp2Stream]>;
    if (pendingStreams.length > 0) {
      const error = createHttp2Error(
        "ERR_HTTP2_GOAWAY_SESSION",
        "The HTTP/2 session is closing before the stream could be established.",
      );
      queueMicrotask(() => {
        for (const [streamId, stream] of pendingStreams) {
          if (http2Streams.get(streamId) !== stream) {
            continue;
          }
          stream.emit("error", error);
          stream.emit("close");
          http2Streams.delete(streamId);
        }
      });
      if (typeof _networkHttp2SessionDestroyRaw !== "undefined") {
        _networkHttp2SessionDestroyRaw.applySync(undefined, [this._sessionId]);
        return;
      }
    }
    _networkHttp2SessionCloseRaw?.applySync(undefined, [this._sessionId]);
    setTimeout(() => {
      if (!http2Sessions.has(this._sessionId)) {
        return;
      }
      this._release();
      this.emit("close");
      http2Sessions.delete(this._sessionId);
      _unregisterHandle?.(`http2:session:${this._sessionId}`);
    }, 50);
  }
  destroy(): void {
    if (typeof _networkHttp2SessionDestroyRaw !== "undefined") {
      _networkHttp2SessionDestroyRaw.applySync(undefined, [this._sessionId]);
      return;
    }
    this.close();
  }
}

class Http2Server extends Http2EventEmitter {
  readonly allowHalfOpen: boolean;
  readonly allowHTTP1: boolean;
  readonly encrypted: boolean;
  readonly _serverId: number;
  listening = false;
  private _address: { address: string; family: string; port: number } | null = null;
  private _options: Record<string, unknown>;
  private _timeoutMs = 0;
  private _waitStarted = false;
  constructor(
    options: Record<string, unknown> | undefined,
    listener: ((req: Http2ServerRequest, res: Http2ServerResponse) => void) | undefined,
    encrypted: boolean,
  ) {
    super();
    this.allowHalfOpen = options?.allowHalfOpen === true;
    this.allowHTTP1 = options?.allowHTTP1 === true;
    this.encrypted = encrypted;
    const initialSettings =
      options?.settings && typeof options.settings === "object" && !Array.isArray(options.settings)
        ? cloneHttp2Settings(options.settings as Http2SettingsRecord)
        : {};
    this._options = {
      ...(options ?? {}),
      settings: initialSettings,
    };
    this._serverId = nextHttp2ServerId++;
    (this as Record<PropertyKey, unknown>)[HTTP2_OPTIONS] = {
      settings: cloneHttp2Settings(initialSettings),
      unknownProtocolTimeout: 10000,
      ...(encrypted ? { ALPNProtocols: ["h2"] } : {}),
    };
    if (listener) {
      this.on("request", listener as unknown as Http2EventListener);
    }
    http2Servers.set(this._serverId, this);
  }
  address(): { address: string; family: string; port: number } | null {
    return this._address;
  }
  _retain(): void {
    if (this._waitStarted || typeof _networkHttp2ServerWaitRaw === "undefined") {
      return;
    }
    this._waitStarted = true;
    void _networkHttp2ServerWaitRaw.apply(undefined, [this._serverId], {
      result: { promise: true },
    }).catch((error) => {
      this.emit("error", error instanceof Error ? error : new Error(String(error)));
    });
  }
  _release(): void {
    this._waitStarted = false;
  }
  setTimeout(timeout: number, callback?: () => void): this {
    this._timeoutMs = normalizeSocketTimeout(timeout);
    if (callback) {
      this.on("timeout", callback);
    }
    return this;
  }
  updateSettings(settings: Record<string, unknown>): this {
    const normalized = validateHttp2Settings(settings);
    const mergedSettings = {
      ...cloneHttp2Settings(this._options.settings as Http2SettingsRecord),
      ...cloneHttp2Settings(normalized),
    };
    this._options = {
      ...this._options,
      settings: mergedSettings,
    };
    const optionsState = (this as Record<PropertyKey, unknown>)[HTTP2_OPTIONS] as {
      settings: Http2SettingsRecord;
    };
    optionsState.settings = cloneHttp2Settings(mergedSettings);
    return this;
  }
  listen(
    portOrOptions?: number | string | null | { port?: unknown; host?: unknown; backlog?: unknown; path?: unknown },
    hostOrCallback?: string | NetServerEventListener,
    backlogOrCallback?: number | NetServerEventListener,
    callback?: NetServerEventListener,
  ): this {
    if (typeof _networkHttp2ServerListenRaw === "undefined") {
      throw new Error(`http2.${this.encrypted ? "createSecureServer" : "createServer"} is not supported in sandbox`);
    }
    const options = normalizeListenArgs(portOrOptions, hostOrCallback, backlogOrCallback, callback);
    if (options.callback) {
      this.once("listening", options.callback);
    }
    const payload = {
      serverId: this._serverId,
      secure: this.encrypted,
      port: options.port,
      host: options.host,
      backlog: options.backlog,
      allowHalfOpen: this.allowHalfOpen,
      allowHTTP1: this._options.allowHTTP1 === true,
      timeout: this._timeoutMs,
      settings: this._options.settings,
      remoteCustomSettings: this._options.remoteCustomSettings,
      tls: this.encrypted
        ? buildSerializedTlsOptions(
            {
              ...this._options,
              ...((portOrOptions && typeof portOrOptions === "object"
                ? portOrOptions
                : {}) as Record<string, unknown>),
            },
            { isServer: true },
          )
        : undefined,
    };
    const result = JSON.parse(
      _networkHttp2ServerListenRaw.applySyncPromise(undefined, [JSON.stringify(payload)]),
    ) as { address?: { address: string; family: string; port: number } | null };
    this._address = result.address ?? null;
    this.listening = true;
    this._retain();
    _registerHandle?.(`http2:server:${this._serverId}`, "http2 server");
    this.emit("listening");
    return this;
  }
  close(callback?: () => void): this {
    if (callback) {
      this.once("close", callback);
    }
    if (!this.listening) {
      this._release();
      queueMicrotask(() => this.emit("close"));
      return this;
    }
    void _networkHttp2ServerCloseRaw?.apply(undefined, [this._serverId], {
      result: { promise: true },
    });
    setTimeout(() => {
      if (!this.listening) {
        return;
      }
      this.listening = false;
      this._release();
      this.emit("close");
      http2Servers.delete(this._serverId);
      _unregisterHandle?.(`http2:server:${this._serverId}`);
    }, 50);
    return this;
  }
}

function createHttp2Server(
  secure: boolean,
  optionsOrListener?: Record<string, unknown> | ((req: Http2ServerRequest, res: Http2ServerResponse) => void),
  maybeListener?: (req: Http2ServerRequest, res: Http2ServerResponse) => void,
): Http2Server {
  const listener =
    typeof optionsOrListener === "function"
      ? optionsOrListener
      : maybeListener;
  const options =
    optionsOrListener && typeof optionsOrListener === "object" && !Array.isArray(optionsOrListener)
      ? optionsOrListener
      : undefined;
  return new Http2Server(options, listener, secure);
}

function connectHttp2(
  authorityOrOptions: unknown,
  optionsOrListener?: Record<string, unknown> | ((session: Http2Session) => void),
  maybeListener?: (session: Http2Session) => void,
): Http2Session {
  if (typeof _networkHttp2SessionConnectRaw === "undefined") {
    throw new Error("http2.connect is not supported in sandbox");
  }
  const { authority, options, listener } = normalizeHttp2ConnectArgs(
    authorityOrOptions,
    optionsOrListener,
    maybeListener,
  );
  if (authority.protocol !== "http:" && authority.protocol !== "https:") {
    throw createHttp2Error(
      "ERR_HTTP2_UNSUPPORTED_PROTOCOL",
      `protocol "${authority.protocol}" is unsupported.`,
    );
  }
  validateHttp2ConnectOptions(options);
  const socketId = options.createConnection
    ? resolveHttp2SocketId((options.createConnection as () => unknown)())
    : undefined;
  const response = JSON.parse(
    _networkHttp2SessionConnectRaw.applySyncPromise(
      undefined,
      [
        JSON.stringify({
          authority: authority.toString(),
          protocol: authority.protocol,
          host: options.host ?? options.hostname ?? authority.hostname,
          port: options.port ?? authority.port,
          localAddress: options.localAddress,
          family: options.family,
          socketId,
          settings: options.settings,
          remoteCustomSettings: options.remoteCustomSettings,
          tls:
            authority.protocol === "https:"
              ? buildSerializedTlsOptions(options, { servername: typeof options.servername === "string" ? options.servername : authority.hostname })
              : undefined,
        }),
      ],
    ),
  ) as { sessionId: number; state?: string };
  const initialState = parseHttp2SessionState(response.state);
  const session = new Http2Session(
    response.sessionId,
    initialState?.socket ?? undefined,
  );
  applyHttp2SessionState(session, initialState);
  session._beginInitialSettingsAck();
  session._retain();
  if (listener) {
    session.once("connect", () => listener(session));
  }
  http2Sessions.set(response.sessionId, session);
  _registerHandle?.(`http2:session:${response.sessionId}`, "http2 session");
  if (authority.protocol === "https:") {
    session.socket.once("secureConnect", () => {});
  }
  return session;
}

function getOrCreateHttp2Session(
  sessionId: number,
  state?: SerializedHttp2SessionState | null,
): Http2Session {
  let session = http2Sessions.get(sessionId);
  if (!session) {
    session = new Http2Session(sessionId, state?.socket ?? undefined);
    http2Sessions.set(sessionId, session);
  }
  applyHttp2SessionState(session, state);
  return session;
}

function queuePendingHttp2ClientStreamEvent(
  streamId: number,
  event: {
    kind: "push" | "responseHeaders" | "data" | "end" | "close" | "error";
    data?: string;
    extraNumber?: number;
  },
): void {
  const pending = pendingHttp2ClientStreamEvents.get(streamId) ?? [];
  pending.push(event);
  pendingHttp2ClientStreamEvents.set(streamId, pending);
}

function schedulePendingHttp2ClientStreamEventsFlush(streamId: number): void {
  if (scheduledHttp2ClientStreamFlushes.has(streamId)) {
    return;
  }
  scheduledHttp2ClientStreamFlushes.add(streamId);
  const flush = () => {
    scheduledHttp2ClientStreamFlushes.delete(streamId);
    flushPendingHttp2ClientStreamEvents(streamId);
  };
  const scheduleImmediate = (globalThis as { setImmediate?: (callback: () => void) => void }).setImmediate;
  if (typeof scheduleImmediate === "function") {
    scheduleImmediate(flush);
    return;
  }
  setTimeout(flush, 0);
}

function flushPendingHttp2ClientStreamEvents(streamId: number): void {
  const stream = http2Streams.get(streamId);
  if (!stream || typeof (stream as { _emitResponseHeaders?: unknown })._emitResponseHeaders !== "function") {
    return;
  }
  const pending = pendingHttp2ClientStreamEvents.get(streamId);
  if (!pending || pending.length === 0) {
    return;
  }
  pendingHttp2ClientStreamEvents.delete(streamId);
  for (const event of pending) {
    if (event.kind === "push") {
      (stream as ClientHttp2Stream)._emitPush(parseHttp2Headers(event.data), event.extraNumber);
      continue;
    }
    if (event.kind === "responseHeaders") {
      (stream as ClientHttp2Stream)._emitResponseHeaders(parseHttp2Headers(event.data));
      continue;
    }
    if (event.kind === "data") {
      (stream as ClientHttp2Stream)._emitDataChunk(event.data);
      continue;
    }
    if (event.kind === "end") {
      (stream as ClientHttp2Stream)._emitEnd();
      continue;
    }
    if (event.kind === "error") {
      stream.emit("error", parseHttp2ErrorPayload(event.data));
      continue;
    }
    if (typeof (stream as { _emitClose?: unknown })._emitClose === "function") {
      (stream as ClientHttp2Stream)._emitClose(event.extraNumber);
    } else {
      stream.emit("close");
    }
    http2Streams.delete(streamId);
  }
}

function http2Dispatch(
  kind: string,
  id: number,
  data?: string,
  extra?: string,
  extraNumber?: string | number,
  extraHeaders?: string,
  flags?: string | number,
): void {
  if (kind === "sessionConnect") {
    const session = http2Sessions.get(id);
    if (!session) return;
    const state = parseHttp2SessionState(data);
    applyHttp2SessionState(session, state);
    if (session.encrypted) {
      session.socket.emit("secureConnect");
    }
    session.emit("connect");
    return;
  }
  if (kind === "sessionClose") {
    const session = http2Sessions.get(id);
    if (!session) return;
    session._release();
    session.emit("close");
    http2Sessions.delete(id);
    _unregisterHandle?.(`http2:session:${id}`);
    return;
  }
  if (kind === "sessionError") {
    const session = http2Sessions.get(id);
    if (!session) return;
    session.emit("error", parseHttp2ErrorPayload(data));
    return;
  }
  if (kind === "sessionLocalSettings") {
    const session = http2Sessions.get(id);
    if (!session) return;
    session._applyLocalSettings(parseHttp2Headers(data) as unknown as Http2SettingsRecord);
    return;
  }
  if (kind === "sessionRemoteSettings") {
    const session = http2Sessions.get(id);
    if (!session) return;
    session._applyRemoteSettings(parseHttp2Headers(data) as unknown as Http2SettingsRecord);
    return;
  }
  if (kind === "sessionSettingsAck") {
    const session = http2Sessions.get(id);
    if (!session) return;
    session._ackSettings();
    return;
  }
  if (kind === "sessionGoaway") {
    const session = http2Sessions.get(id);
    if (!session) return;
    session.emit(
      "goaway",
      Number(extraNumber ?? 0),
      Number(flags ?? 0),
      data ? Buffer.from(data, "base64") : Buffer.alloc(0),
    );
    return;
  }
  if (kind === "clientPushStream") {
    const session = http2Sessions.get(id);
    if (!session) return;
    const streamId = Number(data);
    const stream = new ClientHttp2Stream(streamId, session, true);
    http2Streams.set(streamId, stream);
    session.emit("stream", stream, parseHttp2Headers(extraHeaders), Number(flags ?? 0));
    schedulePendingHttp2ClientStreamEventsFlush(streamId);
    return;
  }
  if (kind === "clientPushHeaders") {
    queuePendingHttp2ClientStreamEvent(id, {
      kind: "push",
      data,
      extraNumber: Number(extraNumber ?? 0),
    });
    schedulePendingHttp2ClientStreamEventsFlush(id);
    return;
  }
  if (kind === "clientResponseHeaders") {
    queuePendingHttp2ClientStreamEvent(id, {
      kind: "responseHeaders",
      data,
    });
    schedulePendingHttp2ClientStreamEventsFlush(id);
    return;
  }
  if (kind === "clientData") {
    queuePendingHttp2ClientStreamEvent(id, {
      kind: "data",
      data,
    });
    schedulePendingHttp2ClientStreamEventsFlush(id);
    return;
  }
  if (kind === "clientEnd") {
    queuePendingHttp2ClientStreamEvent(id, {
      kind: "end",
    });
    schedulePendingHttp2ClientStreamEventsFlush(id);
    return;
  }
  if (kind === "clientClose") {
    queuePendingHttp2ClientStreamEvent(id, {
      kind: "close",
      extraNumber: Number(extraNumber ?? 0),
    });
    schedulePendingHttp2ClientStreamEventsFlush(id);
    return;
  }
  if (kind === "clientError") {
    queuePendingHttp2ClientStreamEvent(id, {
      kind: "error",
      data,
    });
    schedulePendingHttp2ClientStreamEventsFlush(id);
    return;
  }
  if (kind === "serverStream") {
    const server = http2Servers.get(id);
    if (!server) return;
    const sessionState = parseHttp2SessionState(extra);
    const sessionId = Number(extraNumber);
    const session = getOrCreateHttp2Session(sessionId, sessionState);
    const streamId = Number(data);
    const headers = parseHttp2Headers(extraHeaders);
    const numericFlags = Number(flags ?? 0);
    const stream = new ServerHttp2Stream(streamId, session, headers);
    http2Streams.set(streamId, stream);
    server.emit("stream", stream, headers, numericFlags);
    if (server.listenerCount("request") > 0) {
      const request = new Http2ServerRequest(headers, session.socket, stream);
      const response = new Http2ServerResponse(stream);
      stream.on("data", (chunk) => {
        request._emitData(chunk as Buffer);
      });
      stream.on("end", () => {
        request._emitEnd();
      });
      stream.on("error", (error) => {
        request._emitError(error as Error);
      });
      stream.on("drain", () => {
        response.emit("drain");
      });
      server.emit("request", request, response);
    }
    return;
  }
  if (kind === "serverStreamData") {
    const stream = http2Streams.get(id);
    if (!stream || typeof (stream as { _emitData?: unknown })._emitData !== "function") return;
    (stream as ServerHttp2Stream)._emitData(data);
    return;
  }
  if (kind === "serverStreamEnd") {
    const stream = http2Streams.get(id);
    if (!stream || typeof (stream as { _emitEnd?: unknown })._emitEnd !== "function") return;
    (stream as ServerHttp2Stream)._emitEnd();
    return;
  }
  if (kind === "serverStreamDrain") {
    const stream = http2Streams.get(id);
    if (!stream || typeof (stream as { _emitDrain?: unknown })._emitDrain !== "function") return;
    (stream as ServerHttp2Stream)._emitDrain();
    return;
  }
  if (kind === "serverStreamError") {
    const stream = http2Streams.get(id);
    if (!stream) return;
    if (
      typeof (stream as { _shouldSuppressHostError?: unknown })._shouldSuppressHostError === "function" &&
      (stream as ServerHttp2Stream)._shouldSuppressHostError()
    ) {
      return;
    }
    stream.emit("error", parseHttp2ErrorPayload(data));
    return;
  }
  if (kind === "serverStreamClose") {
    const stream = http2Streams.get(id);
    if (!stream || typeof (stream as { _emitClose?: unknown })._emitClose !== "function") return;
    (stream as ServerHttp2Stream)._emitClose(Number(extraNumber ?? 0));
    http2Streams.delete(id);
    return;
  }
  if (kind === "serverSession") {
    const server = http2Servers.get(id);
    if (!server) return;
    const sessionId = Number(extraNumber);
    const session = getOrCreateHttp2Session(sessionId, parseHttp2SessionState(data));
    server.emit("session", session);
    return;
  }
  if (kind === "serverTimeout") {
    http2Servers.get(id)?.emit("timeout");
    return;
  }
  if (kind === "serverConnection") {
    http2Servers.get(id)?.emit("connection", new Http2SocketProxy(parseHttp2SocketState(data) ?? undefined));
    return;
  }
  if (kind === "serverSecureConnection") {
    http2Servers.get(id)?.emit("secureConnection", new Http2SocketProxy(parseHttp2SocketState(data) ?? undefined));
    return;
  }
  if (kind === "serverClose") {
    const server = http2Servers.get(id);
    if (!server) return;
    server.listening = false;
    server._release();
    server.emit("close");
    http2Servers.delete(id);
    _unregisterHandle?.(`http2:server:${id}`);
    return;
  }
  if (kind === "serverCompatRequest") {
    pendingHttp2CompatRequests.set(Number(extraNumber), {
      serverId: id,
      requestJson: data ?? "{}",
    });
    void dispatchHttp2CompatibilityRequest(id, Number(extraNumber));
  }
}

function scheduleQueuedHttp2DispatchDrain(): void {
  if (scheduledHttp2DispatchDrain) {
    return;
  }
  scheduledHttp2DispatchDrain = true;
  const drain = () => {
    scheduledHttp2DispatchDrain = false;
    while (queuedHttp2DispatchEvents.length > 0) {
      const event = queuedHttp2DispatchEvents.shift();
      if (!event) {
        continue;
      }
      http2Dispatch(
        event.kind,
        event.id,
        event.data,
        event.extra,
        event.extraNumber,
        event.extraHeaders,
        event.flags,
      );
    }
  };
  queueMicrotask(drain);
}

function onHttp2Dispatch(_eventType: string, payload?: unknown): void {
  if (!payload || typeof payload !== "object") {
    return;
  }
  const event = payload as {
    kind?: unknown;
    id?: unknown;
    data?: unknown;
    extra?: unknown;
    extraNumber?: unknown;
    extraHeaders?: unknown;
    flags?: unknown;
  };
  if (typeof event.kind !== "string" || typeof event.id !== "number") {
    return;
  }
  if (process.env.SECURE_EXEC_DEBUG_HTTP2_BRIDGE === "1") {
    console.error("[secure-exec http2 isolate dispatch]", event.kind, event.id);
  }
  const kind = event.kind;
  const id = event.id;
  const data = typeof event.data === "string" ? event.data : undefined;
  const extra = typeof event.extra === "string" ? event.extra : undefined;
  const normalizedExtraNumber =
    typeof event.extraNumber === "string" || typeof event.extraNumber === "number"
      ? event.extraNumber
      : undefined;
  const extraHeaders = typeof event.extraHeaders === "string" ? event.extraHeaders : undefined;
  const flags =
    typeof event.flags === "string" || typeof event.flags === "number"
      ? event.flags
      : undefined;
  queuedHttp2DispatchEvents.push({
    kind,
    id,
    data,
    extra,
    extraNumber: normalizedExtraNumber,
    extraHeaders,
    flags,
  });
  scheduleQueuedHttp2DispatchDrain();
}

export const http2 = {
  Http2ServerRequest,
  Http2ServerResponse,
  Http2Stream,
  NghttpError,
  nghttp2ErrorString,
  constants: {
    HTTP2_HEADER_METHOD: ":method",
    HTTP2_HEADER_PATH: ":path",
    HTTP2_HEADER_SCHEME: ":scheme",
    HTTP2_HEADER_AUTHORITY: ":authority",
    HTTP2_HEADER_STATUS: ":status",
    HTTP2_HEADER_CONTENT_TYPE: "content-type",
    HTTP2_HEADER_CONTENT_LENGTH: "content-length",
    HTTP2_HEADER_LAST_MODIFIED: "last-modified",
    HTTP2_HEADER_ACCEPT: "accept",
    HTTP2_HEADER_ACCEPT_ENCODING: "accept-encoding",
    HTTP2_METHOD_GET: "GET",
    HTTP2_METHOD_POST: "POST",
    HTTP2_METHOD_PUT: "PUT",
    HTTP2_METHOD_DELETE: "DELETE",
    ...HTTP2_INTERNAL_BINDING_CONSTANTS,
    DEFAULT_SETTINGS_MAX_HEADER_LIST_SIZE: 65535,
  } as Record<string, string | number>,
  getDefaultSettings(): Http2SettingsRecord {
    return cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
  },
  connect: connectHttp2,
  createServer: createHttp2Server.bind(undefined, false),
  createSecureServer: createHttp2Server.bind(undefined, true),
};

// Export modules and make them available as globals for require()
exposeCustomGlobal("_httpModule", http);
exposeCustomGlobal("_httpsModule", https);
exposeCustomGlobal("_http2Module", http2);
exposeCustomGlobal("_dnsModule", dns);
function onHttpServerRequest(
  eventType: string,
  payload?: {
    serverId?: number;
    requestId?: number;
    request?: string;
  } | null,
): void {
  debugBridgeNetwork("http stream event", eventType, payload);
  if (eventType !== "http_request") {
    return;
  }
  if (!payload || payload.serverId === undefined || payload.requestId === undefined || typeof payload.request !== "string") {
    return;
  }
  if (typeof _networkHttpServerRespondRaw === "undefined") {
    debugBridgeNetwork("http stream missing respond bridge");
    return;
  }

  void dispatchServerRequest(payload.serverId, payload.request)
    .then((responseJson) => {
      debugBridgeNetwork("http stream response", payload.serverId, payload.requestId);
      _networkHttpServerRespondRaw.applySync(undefined, [
        payload.serverId!,
        payload.requestId!,
        responseJson,
      ]);
    })
    .catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      debugBridgeNetwork("http stream error", payload.serverId, payload.requestId, message);
      _networkHttpServerRespondRaw.applySync(undefined, [
        payload.serverId!,
        payload.requestId!,
        JSON.stringify({
          status: 500,
          headers: [["content-type", "text/plain"]],
          body: `Error: ${message}`,
          bodyEncoding: "utf8",
        }),
      ]);
    });
}

exposeCustomGlobal("_httpServerDispatch", onHttpServerRequest);
exposeCustomGlobal("_httpServerUpgradeDispatch", dispatchUpgradeRequest);
exposeCustomGlobal("_httpServerConnectDispatch", dispatchConnectRequest);
exposeCustomGlobal("_http2Dispatch", onHttp2Dispatch);
exposeCustomGlobal("_upgradeSocketData", onUpgradeSocketData);
exposeCustomGlobal("_upgradeSocketEnd", onUpgradeSocketEnd);

// Harden fetch API globals (non-writable, non-configurable)
exposeCustomGlobal("fetch", fetch);
exposeCustomGlobal("Headers", Headers);
exposeCustomGlobal("Request", Request);
exposeCustomGlobal("Response", Response);
if (typeof (globalThis as Record<string, unknown>).Blob === "undefined") {
  // Minimal Blob stub used by server frameworks for instanceof checks.
  exposeCustomGlobal("Blob", class BlobStub {});
}
if (typeof (globalThis as Record<string, unknown>).File === "undefined") {
  class FileStub extends Blob {
    name: string;
    lastModified: number;
    webkitRelativePath: string;

    constructor(
      parts: BlobPart[] = [],
      name = "",
      options: BlobPropertyBag & { lastModified?: number } = {},
    ) {
      super(parts, options);
      this.name = String(name);
      this.lastModified =
        typeof options.lastModified === "number"
          ? options.lastModified
          : Date.now();
      this.webkitRelativePath = "";
    }
  }
  exposeCustomGlobal("File", FileStub);
}
if (typeof (globalThis as Record<string, unknown>).FormData === "undefined") {
  // Minimal FormData stub — server frameworks check `instanceof FormData`.
  class FormDataStub {
    private _entries: [string, string][] = [];
    append(name: string, value: string): void {
      this._entries.push([name, value]);
    }
    get(name: string): string | null {
      const entry = this._entries.find(([k]) => k === name);
      return entry ? entry[1] : null;
    }
    getAll(name: string): string[] {
      return this._entries.filter(([k]) => k === name).map(([, v]) => v);
    }
    has(name: string): boolean {
      return this._entries.some(([k]) => k === name);
    }
    delete(name: string): void {
      this._entries = this._entries.filter(([k]) => k !== name);
    }
    entries(): IterableIterator<[string, string]> {
      return this._entries[Symbol.iterator]();
    }
    [Symbol.iterator](): IterableIterator<[string, string]> {
      return this.entries();
    }
  }
  exposeCustomGlobal("FormData", FormDataStub);
}

// ===================================================================
// net module — TCP socket support bridged to the host
// ===================================================================

type NetEventListener = (...args: unknown[]) => void;

const NET_SOCKET_REGISTRY_PREFIX = "__secureExecNetSocket:";
const NET_SERVER_HANDLE_PREFIX = "net-server:";

type NetSocketInfo = {
  localAddress: string;
  localPort: number;
  localFamily: string;
  localPath?: string;
  remoteAddress?: string;
  remotePort?: number;
  remoteFamily?: string;
  remotePath?: string;
};

type SerializedTlsDataValue =
  | {
      kind: "buffer";
      data: string;
    }
  | {
      kind: "string";
      data: string;
    };

type SerializedTlsMaterial = SerializedTlsDataValue | SerializedTlsDataValue[];

type SerializedTlsBridgeOptions = {
  isServer?: boolean;
  servername?: string;
  rejectUnauthorized?: boolean;
  requestCert?: boolean;
  session?: string;
  key?: SerializedTlsMaterial;
  cert?: SerializedTlsMaterial;
  ca?: SerializedTlsMaterial;
  passphrase?: string;
  ciphers?: string;
  ALPNProtocols?: string[];
  minVersion?: string;
  maxVersion?: string;
};

type SerializedTlsClientHello = {
  servername?: string;
  ALPNProtocols?: string[];
};

type TlsSecureContextWrapper = {
  __secureExecTlsContext: SerializedTlsBridgeOptions;
  context: Record<string, unknown>;
};

type SerializedTlsState = {
  authorized?: boolean;
  authorizationError?: string;
  alpnProtocol?: string | false;
  servername?: string;
  protocol?: string | null;
  sessionReused?: boolean;
  cipher?: {
    name?: string;
    standardName?: string;
    version?: string;
  } | null;
};

type SerializedTlsBridgeValue =
  | null
  | boolean
  | number
  | string
  | {
      type: "undefined";
    }
  | {
      type: "buffer";
      data: string;
    }
  | {
      type: "array";
      value: SerializedTlsBridgeValue[];
    }
  | {
      type: "object";
      id: number;
      value: Record<string, SerializedTlsBridgeValue>;
    }
  | {
      type: "ref";
      id: number;
    };

type SerializedTlsError = {
  message: string;
  name?: string;
  code?: string;
  stack?: string;
  authorized?: boolean;
  authorizationError?: string;
};

type NetSocketHandle = {
  setNoDelay?: (enable?: boolean) => unknown;
  setKeepAlive?: (enable?: boolean, initialDelay?: number) => unknown;
  readStart?: () => unknown;
  ref?: () => unknown;
  unref?: () => unknown;
  socketId?: number;
};

type AcceptedNetClientHandle = NetSocketHandle & {
  socketId: number;
  info: NetSocketInfo;
};

function getRegisteredNetSocket(socketId: number): NetSocket | undefined {
  return (globalThis as Record<string, unknown>)[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`] as NetSocket | undefined;
}

function registerNetSocket(socketId: number, socket: NetSocket): void {
  (globalThis as Record<string, unknown>)[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`] = socket;
}

function unregisterNetSocket(socketId: number): void {
  delete (globalThis as Record<string, unknown>)[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`];
}

function isTruthySocketOption(value: unknown): boolean {
  return value === undefined ? true : Boolean(value);
}

function normalizeKeepAliveDelay(initialDelay?: number): number {
  if (typeof initialDelay !== "number" || !Number.isFinite(initialDelay)) {
    return 0;
  }
  return Math.max(0, Math.floor(initialDelay / 1000));
}

function createTimeoutArgTypeError(argumentName: string, value: unknown): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The "${argumentName}" argument must be of type number. Received ${formatReceivedType(value)}`,
    "ERR_INVALID_ARG_TYPE",
  );
}

function createFunctionArgTypeError(argumentName: string, value: unknown): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The "${argumentName}" argument must be of type function. Received ${formatReceivedType(value)}`,
    "ERR_INVALID_ARG_TYPE",
  );
}

function createTimeoutRangeError(value: number): RangeError & { code: string } {
  const error = new RangeError(
    `The value of "timeout" is out of range. It must be a non-negative finite number. Received ${String(value)}`,
  ) as RangeError & { code: string };
  error.code = "ERR_OUT_OF_RANGE";
  return error;
}

function createListenArgValueError(message: string): TypeError & { code: string } {
  return createTypeErrorWithCode(message, "ERR_INVALID_ARG_VALUE");
}

function createSocketBadPortError(value: unknown): RangeError & { code: string } {
  const error = new RangeError(
    `options.port should be >= 0 and < 65536. Received ${formatReceivedType(value)}.`,
  ) as RangeError & { code: string };
  error.code = "ERR_SOCKET_BAD_PORT";
  return error;
}

function isValidTcpPort(value: number): boolean {
  return Number.isInteger(value) && value >= 0 && value < 65536;
}

function isDecimalIntegerString(value: string): boolean {
  return /^[0-9]+$/.test(value);
}

function normalizeListenPortValue(value: unknown): number {
  if (value === undefined || value === null) {
    return 0;
  }
  if (typeof value === "string" && value.length > 0) {
    const parsed = Number(value);
    if (isValidTcpPort(parsed)) {
      return parsed;
    }
    throw createSocketBadPortError(value);
  }
  if (typeof value === "number") {
    if (isValidTcpPort(value)) {
      return value;
    }
    throw createSocketBadPortError(value);
  }
  throw createListenArgValueError(
    `The argument 'options' is invalid. Received ${String(value)}`,
  );
}

type ParsedListenOptions = {
  port?: number;
  host?: string;
  path?: string;
  backlog: number;
  readableAll: boolean;
  writableAll: boolean;
  callback?: NetServerEventListener;
};

function normalizeListenArgs(
  portOrOptions?: number | string | null | { port?: unknown; host?: unknown; backlog?: unknown; path?: unknown },
  hostOrCallback?: string | NetServerEventListener,
  backlogOrCallback?: number | NetServerEventListener,
  callback?: NetServerEventListener,
): ParsedListenOptions {
  const defaultOptions = {
    port: 0,
    host: "127.0.0.1",
    backlog: 511,
    readableAll: false,
    writableAll: false,
  };

  if (typeof portOrOptions === "function") {
    return {
      ...defaultOptions,
      callback: portOrOptions,
    };
  }

  if (portOrOptions !== null && typeof portOrOptions === "object") {
    const options = portOrOptions as {
      port?: unknown;
      host?: unknown;
      backlog?: unknown;
      path?: unknown;
      readableAll?: unknown;
      writableAll?: unknown;
    };
    const hasPort = Object.prototype.hasOwnProperty.call(options, "port");
    const hasPath = Object.prototype.hasOwnProperty.call(options, "path");
    if (!hasPort && !hasPath) {
      throw createListenArgValueError(
        `The argument 'options' must have the property "port" or "path". Received ${String(portOrOptions)}`,
      );
    }
    if (hasPort && hasPath) {
      throw createListenArgValueError(
        `The argument 'options' is invalid. Received ${String(portOrOptions)}`,
      );
    }

    if (
      hasPort &&
      options.port !== undefined &&
      options.port !== null &&
      typeof options.port !== "number" &&
      typeof options.port !== "string"
    ) {
      throw createListenArgValueError(
        `The argument 'options' is invalid. Received ${String(portOrOptions)}`,
      );
    }

    if (hasPath) {
      if (typeof options.path !== "string" || options.path.length === 0) {
        throw createListenArgValueError(
          `The argument 'options' is invalid. Received ${String(portOrOptions)}`,
        );
      }
      return {
        path: options.path,
        backlog:
          typeof options.backlog === "number" && Number.isFinite(options.backlog)
            ? options.backlog
            : defaultOptions.backlog,
        readableAll: options.readableAll === true,
        writableAll: options.writableAll === true,
        callback:
          typeof hostOrCallback === "function"
            ? hostOrCallback
            : typeof backlogOrCallback === "function"
              ? backlogOrCallback
              : callback,
      };
    }

    return {
      port: normalizeListenPortValue(options.port),
      host:
        typeof options.host === "string" && options.host.length > 0
          ? options.host
          : defaultOptions.host,
      backlog:
        typeof options.backlog === "number" && Number.isFinite(options.backlog)
          ? options.backlog
          : defaultOptions.backlog,
      readableAll: false,
      writableAll: false,
      callback:
        typeof hostOrCallback === "function"
          ? hostOrCallback
          : typeof backlogOrCallback === "function"
            ? backlogOrCallback
            : callback,
    };
  }

  if (
    portOrOptions !== undefined &&
    portOrOptions !== null &&
    typeof portOrOptions !== "number" &&
    typeof portOrOptions !== "string"
  ) {
    throw createListenArgValueError(
      `The argument 'options' is invalid. Received ${String(portOrOptions)}`,
    );
  }

  if (typeof portOrOptions === "string" && portOrOptions.length > 0 && !isDecimalIntegerString(portOrOptions)) {
    return {
      path: portOrOptions,
      backlog: defaultOptions.backlog,
      readableAll: false,
      writableAll: false,
      callback:
        typeof hostOrCallback === "function"
          ? hostOrCallback
          : typeof backlogOrCallback === "function"
            ? backlogOrCallback
            : callback,
    };
  }

  return {
    port: normalizeListenPortValue(portOrOptions),
    host: typeof hostOrCallback === "string" ? hostOrCallback : defaultOptions.host,
    backlog: typeof backlogOrCallback === "number" ? backlogOrCallback : defaultOptions.backlog,
    readableAll: false,
    writableAll: false,
    callback:
      typeof hostOrCallback === "function"
        ? hostOrCallback
        : typeof backlogOrCallback === "function"
          ? backlogOrCallback
          : callback,
  };
}

type ParsedConnectOptions = {
  host?: string;
  port?: number;
  path?: string;
  keepAlive?: unknown;
  keepAliveInitialDelay?: number;
  callback?: () => void;
};

function normalizeConnectArgs(
  portOrOptions:
    | number
    | string
    | {
        host?: string;
        port?: number;
        path?: string;
        keepAlive?: unknown;
        keepAliveInitialDelay?: number;
      },
  hostOrCallback?: string | (() => void),
  callback?: () => void,
): ParsedConnectOptions {
  if (portOrOptions !== null && typeof portOrOptions === "object") {
    return {
      host:
        typeof portOrOptions.host === "string" && portOrOptions.host.length > 0
          ? portOrOptions.host
          : undefined,
      port: portOrOptions.port,
      path:
        typeof portOrOptions.path === "string" && portOrOptions.path.length > 0
          ? portOrOptions.path
          : undefined,
      keepAlive: portOrOptions.keepAlive,
      keepAliveInitialDelay: portOrOptions.keepAliveInitialDelay,
      callback: typeof hostOrCallback === "function" ? hostOrCallback : callback,
    };
  }

  if (typeof portOrOptions === "string" && !isDecimalIntegerString(portOrOptions)) {
    return {
      path: portOrOptions,
      callback: typeof hostOrCallback === "function" ? hostOrCallback : callback,
    };
  }

  return {
    port: typeof portOrOptions === "number" ? portOrOptions : Number(portOrOptions),
    host: typeof hostOrCallback === "string" ? hostOrCallback : "127.0.0.1",
    callback: typeof hostOrCallback === "function" ? hostOrCallback : callback,
  };
}

function isValidIPv4Segment(segment: string): boolean {
  if (!/^[0-9]{1,3}$/.test(segment)) {
    return false;
  }
  if (segment.length > 1 && segment.startsWith("0")) {
    return false;
  }
  const value = Number(segment);
  return Number.isInteger(value) && value >= 0 && value <= 255;
}

function isIPv4String(input: string): boolean {
  const segments = input.split(".");
  return segments.length === 4 && segments.every((segment) => isValidIPv4Segment(segment));
}

function isValidIPv6Zone(zone: string): boolean {
  return zone.length > 0 && /^[0-9A-Za-z_.-]+$/.test(zone);
}

function countIPv6Parts(part: string): number | null {
  if (part.length === 0) {
    return 0;
  }
  const segments = part.split(":");
  let count = 0;
  for (const segment of segments) {
    if (segment.length === 0) {
      return null;
    }
    if (segment.includes(".")) {
      if (segment !== segments[segments.length - 1] || !isIPv4String(segment)) {
        return null;
      }
      count += 2;
      continue;
    }
    if (!/^[0-9A-Fa-f]{1,4}$/.test(segment)) {
      return null;
    }
    count += 1;
  }
  return count;
}

function isIPv6String(input: string): boolean {
  if (input.length === 0) {
    return false;
  }

  let address = input;
  const zoneIndex = address.indexOf("%");
  if (zoneIndex !== -1) {
    if (address.indexOf("%", zoneIndex + 1) !== -1) {
      return false;
    }
    const zone = address.slice(zoneIndex + 1);
    if (!isValidIPv6Zone(zone)) {
      return false;
    }
    address = address.slice(0, zoneIndex);
  }

  const doubleColonIndex = address.indexOf("::");
  if (doubleColonIndex !== -1) {
    if (address.indexOf("::", doubleColonIndex + 2) !== -1) {
      return false;
    }
    const [left, right] = address.split("::");
    if (left.includes(".")) {
      return false;
    }
    const leftCount = countIPv6Parts(left);
    const rightCount = countIPv6Parts(right);
    if (leftCount === null || rightCount === null) {
      return false;
    }
    return leftCount + rightCount < 8;
  }

  const count = countIPv6Parts(address);
  return count === 8;
}

function coerceIpInput(input: unknown): string {
  if (input === null || input === undefined) {
    return "";
  }
  return String(input);
}

function classifyIpAddress(input: unknown): 0 | 4 | 6 {
  const value = coerceIpInput(input);
  if (isIPv4String(value)) {
    return 4;
  }
  if (isIPv6String(value)) {
    return 6;
  }
  return 0;
}

function normalizeSocketTimeout(timeout: unknown): number {
  if (typeof timeout !== "number") {
    throw createTimeoutArgTypeError("timeout", timeout);
  }
  if (!Number.isFinite(timeout) || timeout < 0) {
    throw createTimeoutRangeError(timeout);
  }
  return timeout;
}

function parseNetSocketInfo(data?: string): NetSocketInfo | null {
  if (!data) {
    return null;
  }
  try {
    const parsed = JSON.parse(data) as NetSocketInfo;
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

function serializeTlsValue(value: unknown): SerializedTlsMaterial | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  if (Array.isArray(value)) {
    const entries = value
      .map((entry) => serializeTlsValue(entry))
      .flatMap((entry) => Array.isArray(entry) ? entry : entry ? [entry] : []);
    return entries.length > 0 ? entries : undefined;
  }
  if (typeof value === "string") {
    return { kind: "string", data: value };
  }
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    return { kind: "buffer", data: Buffer.from(value).toString("base64") };
  }
  return undefined;
}

function isTlsSecureContextWrapper(value: unknown): value is TlsSecureContextWrapper {
  return !!value &&
    typeof value === "object" &&
    "__secureExecTlsContext" in (value as Record<string, unknown>);
}

function buildSerializedTlsOptions(
  options: Record<string, unknown> | undefined,
  extra?: Partial<SerializedTlsBridgeOptions>,
): SerializedTlsBridgeOptions {
  const contextOptions = isTlsSecureContextWrapper(options?.secureContext)
    ? options.secureContext.__secureExecTlsContext
    : undefined;
  const serialized: SerializedTlsBridgeOptions = {
    ...(contextOptions ?? {}),
    ...extra,
  };
  const key = serializeTlsValue(options?.key);
  const cert = serializeTlsValue(options?.cert);
  const ca = serializeTlsValue(options?.ca);
  if (key !== undefined) serialized.key = key;
  if (cert !== undefined) serialized.cert = cert;
  if (ca !== undefined) serialized.ca = ca;
  if (typeof options?.passphrase === "string") serialized.passphrase = options.passphrase;
  if (typeof options?.ciphers === "string") serialized.ciphers = options.ciphers;
  if (Buffer.isBuffer(options?.session) || options?.session instanceof Uint8Array) {
    serialized.session = Buffer.from(options.session).toString("base64");
  }
  if (Array.isArray(options?.ALPNProtocols)) {
    const protocols = options.ALPNProtocols
      .filter((value): value is string => typeof value === "string");
    if (protocols.length > 0) {
      serialized.ALPNProtocols = protocols;
    }
  }
  if (typeof options?.minVersion === "string") serialized.minVersion = options.minVersion;
  if (typeof options?.maxVersion === "string") serialized.maxVersion = options.maxVersion;
  if (typeof options?.servername === "string") serialized.servername = options.servername;
  if (typeof options?.rejectUnauthorized === "boolean") {
    serialized.rejectUnauthorized = options.rejectUnauthorized;
  }
  if (typeof options?.requestCert === "boolean") {
    serialized.requestCert = options.requestCert;
  }
  return serialized;
}

function parseTlsState(payload?: string): SerializedTlsState | null {
  if (!payload) {
    return null;
  }
  try {
    return JSON.parse(payload) as SerializedTlsState;
  } catch {
    return null;
  }
}

function parseTlsClientHello(payload?: string): SerializedTlsClientHello | null {
  if (!payload) {
    return null;
  }
  try {
    return JSON.parse(payload) as SerializedTlsClientHello;
  } catch {
    return null;
  }
}

function createBridgedTlsError(payload?: string): Error {
  if (!payload) {
    return new Error("socket error");
  }
  try {
    const parsed = JSON.parse(payload) as SerializedTlsError;
    const error = new Error(parsed.message);
    if (parsed.name) error.name = parsed.name;
    if (parsed.code) {
      (error as Error & { code?: string }).code = parsed.code;
    }
    if (parsed.stack) error.stack = parsed.stack;
    return error;
  } catch {
    return new Error(payload);
  }
}

function deserializeTlsBridgeValue(
  value: SerializedTlsBridgeValue,
  refs = new Map<number, Record<string, unknown>>(),
): unknown {
  if (
    value === null ||
    typeof value === "boolean" ||
    typeof value === "number" ||
    typeof value === "string"
  ) {
    return value;
  }
  if (value.type === "undefined") {
    return undefined;
  }
  if (value.type === "buffer") {
    return Buffer.from(value.data, "base64");
  }
  if (value.type === "array") {
    return value.value.map((entry) => deserializeTlsBridgeValue(entry, refs));
  }
  if (value.type === "ref") {
    return refs.get(value.id);
  }
  const target: Record<string, unknown> = {};
  refs.set(value.id, target);
  for (const [key, entry] of Object.entries(value.value)) {
    target[key] = deserializeTlsBridgeValue(entry, refs);
  }
  return target;
}

function queryTlsSocket(
  socketId: number,
  query: string,
  detailed?: boolean,
): unknown {
  if (typeof _netSocketTlsQueryRaw === "undefined") {
    return undefined;
  }
  const payload = _netSocketTlsQueryRaw.applySync(
    undefined,
    detailed === undefined ? [socketId, query] : [socketId, query, detailed],
  );
  return deserializeTlsBridgeValue(JSON.parse(payload) as SerializedTlsBridgeValue);
}

function createConnectedSocketHandle(socketId: number): NetSocketHandle {
  return {
    socketId,
    setNoDelay(enable?: boolean) {
      _netSocketSetNoDelayRaw?.applySync(undefined, [socketId, enable !== false]);
      return this;
    },
    setKeepAlive(enable?: boolean, initialDelay?: number) {
      _netSocketSetKeepAliveRaw?.applySync(undefined, [
        socketId,
        enable !== false,
        normalizeKeepAliveDelay(initialDelay),
      ]);
      return this;
    },
    ref() {
      return this;
    },
    unref() {
      return this;
    },
  };
}

function createAcceptedClientHandle(
  socketId: number,
  info: NetSocketInfo,
): AcceptedNetClientHandle {
  return {
    socketId,
    info,
    setNoDelay(enable?: boolean) {
      _netSocketSetNoDelayRaw?.applySync(undefined, [socketId, enable !== false]);
      return this;
    },
    setKeepAlive(enable?: boolean, initialDelay?: number) {
      _netSocketSetKeepAliveRaw?.applySync(undefined, [
        socketId,
        enable !== false,
        normalizeKeepAliveDelay(initialDelay),
      ]);
      return this;
    },
    ref() {
      return this;
    },
    unref() {
      return this;
    },
  };
}

const NET_BRIDGE_TIMEOUT_SENTINEL = "__secure_exec_net_timeout__";
const NET_BRIDGE_POLL_DELAY_MS = 10;

// Dispatch callback invoked by the host when socket events arrive
function netSocketDispatch(socketId: number, event: string, data?: string): void {
  if (socketId === 0 && event.startsWith("http2:")) {
    debugBridgeNetwork("http2 dispatch via netSocket", event);
    try {
      const payload = data ? JSON.parse(data) as {
        id?: number;
        data?: string;
        extra?: string;
        extraNumber?: string | number;
        extraHeaders?: string;
        flags?: string | number;
      } : {};
      http2Dispatch(
        event.slice("http2:".length),
        Number(payload.id ?? 0),
        payload.data,
        payload.extra,
        payload.extraNumber,
        payload.extraHeaders,
        payload.flags,
      );
    } catch {
      // Ignore malformed bridged HTTP/2 dispatch payloads.
    }
    return;
  }
  const socket = getRegisteredNetSocket(socketId);
  if (!socket) return;

  switch (event) {
    case "connect": {
      socket._applySocketInfo(parseNetSocketInfo(data));
      socket._connected = true;
      socket.connecting = false;
      socket._touchTimeout();
      socket._emitNet("connect");
      socket._emitNet("ready");
      break;
    }
    case "secureConnect":
    case "secure": {
      const state = parseTlsState(data);
      socket.encrypted = true;
      if (state) {
        socket.authorized = state.authorized === true;
        socket.authorizationError = state.authorizationError;
        socket.alpnProtocol = state.alpnProtocol ?? false;
        socket.servername = state.servername ?? socket.servername;
        socket._tlsProtocol = state.protocol ?? null;
        socket._tlsSessionReused = state.sessionReused === true;
        socket._tlsCipher = state.cipher ?? null;
      }
      socket._emitNet(event);
      break;
    }
    case "data": {
      const buf = typeof Buffer !== "undefined"
        ? Buffer.from(data!, "base64")
        : new Uint8Array(0);
      socket._touchTimeout();
      socket._emitNet("data", buf);
      break;
    }
    case "end":
      socket._emitNet("end");
      break;
    case "session": {
      const session = typeof Buffer !== "undefined"
        ? Buffer.from(data ?? "", "base64")
        : new Uint8Array(0);
      socket._tlsSession = Buffer.from(session);
      socket._emitNet("session", session);
      break;
    }
    case "error":
      if (data) {
        try {
          const parsed = JSON.parse(data) as SerializedTlsError;
          socket.authorized = parsed.authorized === true;
          socket.authorizationError = parsed.authorizationError;
        } catch {
          // Ignore non-JSON payloads.
        }
      }
      socket._emitNet("error", createBridgedTlsError(data));
      break;
    case "close":
      unregisterNetSocket(socketId);
      socket._connected = false;
      socket.connecting = false;
      socket._clearTimeoutTimer();
      socket._emitNet("close");
      break;
  }
}

exposeCustomGlobal("_netSocketDispatch", netSocketDispatch);

class NetSocket {
  private _listeners: Record<string, NetEventListener[]> = {};
  private _onceListeners: Record<string, NetEventListener[]> = {};
  private _socketId = 0;
  private _loopbackServer: Server | null = null;
  private _loopbackBuffer: Buffer = Buffer.alloc(0);
  private _loopbackDispatchRunning = false;
  private _loopbackReadableEnded = false;
  private _loopbackEventQueue: Promise<void> = Promise.resolve();
  private _encoding?: BufferEncoding;
  private _noDelayState = false;
  private _keepAliveState = false;
  private _keepAliveDelaySeconds = 0;
  private _refed = true;
  private _bridgeReadLoopRunning = false;
  private _bridgeReadPollTimer: ReturnType<typeof setTimeout> | null = null;
  private _timeoutMs = 0;
  private _timeoutTimer: ReturnType<typeof setTimeout> | null = null;
  private _tlsUpgrading = false;
  _connected = false;
  connecting = false;
  destroyed = false;
  writable = true;
  readable = true;
  readableLength = 0;
  writableLength = 0;
  remoteAddress?: string;
  remotePort?: number;
  remoteFamily?: string;
  localAddress = "0.0.0.0";
  localPort = 0;
  localFamily = "IPv4";
  localPath?: string;
  remotePath?: string;
  bytesRead = 0;
  bytesWritten = 0;
  bufferSize = 0;
  pending = true;
  allowHalfOpen = false;
  encrypted = false;
  authorized = false;
  authorizationError?: string;
  servername?: string;
  alpnProtocol: string | false = false;
  writableHighWaterMark = 16 * 1024;
  server?: NetServer;
  _tlsCipher: SerializedTlsState["cipher"] = null;
  _tlsProtocol: string | null = null;
  _tlsSession: Buffer | null = null;
  _tlsSessionReused = false;
  // Readable stream state stub for library compatibility
  _readableState = { endEmitted: false };
  _handle: NetSocketHandle | null = null;

  constructor(options?: { allowHalfOpen?: boolean; handle?: NetSocketHandle | null }) {
    if (options?.allowHalfOpen) this.allowHalfOpen = true;
    if (options?.handle) this._handle = options.handle;
  }

  connect(
    portOrOptions:
      | number
      | string
      | {
          host?: string;
          port?: number;
          path?: string;
          keepAlive?: unknown;
          keepAliveInitialDelay?: number;
        },
    hostOrCallback?: string | (() => void),
    callback?: () => void,
  ): this {
    if (typeof _netSocketConnectRaw === "undefined") {
      throw new Error("net.Socket is not supported in sandbox (bridge not available)");
    }

    const {
      host = "127.0.0.1",
      port = 0,
      path,
      keepAlive,
      keepAliveInitialDelay,
      callback: cb,
    } = normalizeConnectArgs(portOrOptions, hostOrCallback, callback);

    if (cb) this.once("connect", cb);

    this.connecting = true;
    this.remoteAddress = path ?? host;
    this.remotePort = path ? undefined : port;
    this.remotePath = path;
    this.pending = false;

    const loopbackServer =
      !path && isLoopbackRequestHost(host)
        ? findLoopbackHttpServerByPort(port)
        : null;
    if (loopbackServer) {
      this._loopbackServer = loopbackServer;
      this._connected = true;
      this.connecting = false;
      queueMicrotask(() => {
        this._touchTimeout();
        this._emitNet("connect");
        this._emitNet("ready");
      });
      return this;
    }

    this._socketId = _netSocketConnectRaw.applySync(
      undefined,
      [JSON.stringify(path ? { path } : { host, port })],
    ) as number;
    this._handle = createConnectedSocketHandle(this._socketId);
    registerNetSocket(this._socketId, this);
    void this._waitForConnect();

    // Note: do NOT use _registerHandle for net sockets — _waitForActiveHandles()
    // blocks dispatch callbacks. Libraries use their own async patterns (Promises,
    // callbacks) which keep the execution alive via the script result promise.

    if (keepAlive) {
      this.once("connect", () => {
        this.setKeepAlive(true, keepAliveInitialDelay);
      });
    }

    return this;
  }

  write(data: unknown, encodingOrCallback?: string | (() => void), callback?: () => void): boolean {
    let buf: Buffer;
    if (Buffer.isBuffer(data)) {
      buf = data;
    } else if (typeof data === "string") {
      const enc = typeof encodingOrCallback === "string" ? encodingOrCallback : "utf-8";
      buf = Buffer.from(data, enc as BufferEncoding);
    } else {
      buf = Buffer.from(data as Uint8Array);
    }

    if (this._loopbackServer) {
      this.bytesWritten += buf.length;
      this._loopbackBuffer = Buffer.concat([this._loopbackBuffer, buf]);
      this._touchTimeout();
      this._dispatchLoopbackHttpRequest();
      const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      if (cb) cb();
      return true;
    }

    if (typeof _netSocketWriteRaw === "undefined") return false;
    if (this.destroyed || !this._socketId) return false;

    const base64 = buf.toString("base64");
    this.bytesWritten += buf.length;
    _netSocketWriteRaw.applySync(undefined, [this._socketId, base64]);
    this._touchTimeout();

    const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    if (cb) cb();
    return true;
  }

  end(dataOrCallback?: unknown, encodingOrCallback?: string | (() => void), callback?: () => void): this {
    if (typeof dataOrCallback === "function") {
      this.once("finish", dataOrCallback as () => void);
    } else if (dataOrCallback != null) {
      this.write(dataOrCallback, encodingOrCallback, callback);
    }
    if (this._loopbackServer) {
      if (!this._loopbackReadableEnded) {
        queueMicrotask(() => {
          this._closeLoopbackReadable();
        });
      }
      return this;
    }
    if (typeof _netSocketEndRaw !== "undefined" && this._socketId && !this.destroyed) {
      _netSocketEndRaw.applySync(undefined, [this._socketId]);
      this._touchTimeout();
    }
    return this;
  }

  destroy(error?: Error): this {
    if (this.destroyed) return this;
    this.destroyed = true;
    this.writable = false;
    this.readable = false;
    this._clearTimeoutTimer();
    if (this._bridgeReadPollTimer) {
      clearTimeout(this._bridgeReadPollTimer);
      this._bridgeReadPollTimer = null;
    }
    if (this._loopbackServer) {
      this._loopbackServer = null;
      if (error) {
        this._emitNet("error", error);
      }
      this._emitNet("close");
      return this;
    }
    if (typeof _netSocketDestroyRaw !== "undefined" && this._socketId) {
      _netSocketDestroyRaw.applySync(undefined, [this._socketId]);
      unregisterNetSocket(this._socketId);
    }
    if (error) {
      this._emitNet("error", error);
    }
    this._emitNet("close");
    return this;
  }

  _applySocketInfo(info: NetSocketInfo | null): void {
    if (!info) {
      return;
    }
    this.localAddress = info.localAddress;
    this.localPort = info.localPort;
    this.localFamily = info.localFamily;
    this.localPath = info.localPath;
    this.remoteAddress = info.remoteAddress ?? this.remoteAddress;
    this.remotePort = info.remotePort ?? this.remotePort;
    this.remoteFamily = info.remoteFamily ?? this.remoteFamily;
    this.remotePath = info.remotePath ?? this.remotePath;
  }

  _applyAcceptedKeepAlive(initialDelay?: number): void {
    this._keepAliveState = true;
    this._keepAliveDelaySeconds = normalizeKeepAliveDelay(initialDelay);
  }

  static fromAcceptedHandle(
    handle: AcceptedNetClientHandle,
    options?: { allowHalfOpen?: boolean },
  ): NetSocket {
    const socket = new NetSocket({ allowHalfOpen: options?.allowHalfOpen });
    socket._socketId = handle.socketId;
    socket._handle = createConnectedSocketHandle(handle.socketId);
    socket._applySocketInfo(handle.info);
    socket._connected = true;
    socket.connecting = false;
    socket.pending = false;
    registerNetSocket(handle.socketId, socket);
    queueMicrotask(() => {
      if (!socket.destroyed && !socket._tlsUpgrading) {
        void socket._pumpBridgeReads();
      }
    });
    return socket;
  }

  setKeepAlive(enable?: boolean, initialDelay?: number): this {
    const nextEnable = isTruthySocketOption(enable);
    const nextDelaySeconds = normalizeKeepAliveDelay(initialDelay);
    if (
      nextEnable === this._keepAliveState &&
      (!nextEnable || nextDelaySeconds === this._keepAliveDelaySeconds)
    ) {
      return this;
    }
    this._keepAliveState = nextEnable;
    this._keepAliveDelaySeconds = nextEnable ? nextDelaySeconds : 0;
    this._handle?.setKeepAlive?.(nextEnable, nextDelaySeconds);
    return this;
  }

  setNoDelay(noDelay?: boolean): this {
    const nextState = isTruthySocketOption(noDelay);
    if (nextState === this._noDelayState) {
      return this;
    }
    this._noDelayState = nextState;
    this._handle?.setNoDelay?.(nextState);
    return this;
  }
  setTimeout(timeout: number, callback?: () => void): this {
    const nextTimeout = normalizeSocketTimeout(timeout);
    if (callback !== undefined && typeof callback !== "function") {
      throw createFunctionArgTypeError("callback", callback);
    }
    if (callback) {
      this.once("timeout", callback);
    }
    this._timeoutMs = nextTimeout;
    if (nextTimeout === 0) {
      this._clearTimeoutTimer();
      return this;
    }
    this._touchTimeout();
    return this;
  }
  ref(): this {
    this._refed = true;
    this._handle?.ref?.();
    if (this._timeoutTimer && typeof this._timeoutTimer.ref === "function") {
      this._timeoutTimer.ref();
    }
    if (
      !this.destroyed &&
      this._connected &&
      !this._loopbackServer &&
      !this._bridgeReadLoopRunning
    ) {
      void this._pumpBridgeReads();
    }
    return this;
  }
  unref(): this {
    this._refed = false;
    this._handle?.unref?.();
    if (this._timeoutTimer && typeof this._timeoutTimer.unref === "function") {
      this._timeoutTimer.unref();
    }
    if (this._bridgeReadPollTimer) {
      clearTimeout(this._bridgeReadPollTimer);
      this._bridgeReadPollTimer = null;
    }
    return this;
  }
  pause(): this { return this; }
  resume(): this { return this; }
  address(): { port: number; family: string; address: string } {
    return { port: this.localPort, family: this.localFamily, address: this.localAddress };
  }
  getCipher(): SerializedTlsState["cipher"] {
    return (queryTlsSocket(this._socketId, "getCipher") as SerializedTlsState["cipher"] | undefined) ?? this._tlsCipher;
  }
  getSession(): Buffer | null {
    const session = queryTlsSocket(this._socketId, "getSession");
    if (Buffer.isBuffer(session)) {
      this._tlsSession = Buffer.from(session);
      return Buffer.from(session);
    }
    return this._tlsSession ? Buffer.from(this._tlsSession) : null;
  }
  isSessionReused(): boolean {
    const reused = queryTlsSocket(this._socketId, "isSessionReused");
    return typeof reused === "boolean" ? reused : this._tlsSessionReused;
  }
  getPeerCertificate(detailed?: boolean): Record<string, unknown> {
    const cert = queryTlsSocket(this._socketId, "getPeerCertificate", detailed === true);
    return cert && typeof cert === "object" ? cert as Record<string, unknown> : {};
  }
  getCertificate(): Record<string, unknown> {
    const cert = queryTlsSocket(this._socketId, "getCertificate");
    return cert && typeof cert === "object" ? cert as Record<string, unknown> : {};
  }
  getProtocol(): string | null {
    const protocol = queryTlsSocket(this._socketId, "getProtocol");
    return typeof protocol === "string" ? protocol : this._tlsProtocol;
  }
  setEncoding(encoding: string): this {
    this._encoding = encoding as BufferEncoding;
    return this;
  }
  pipe<T>(destination: T): T { return destination; }

  on(event: string, listener: NetEventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  addListener(event: string, listener: NetEventListener): this {
    return this.on(event, listener);
  }

  once(event: string, listener: NetEventListener): this {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    return this;
  }

  removeListener(event: string, listener: NetEventListener): this {
    const listeners = this._listeners[event];
    if (listeners) {
      const idx = listeners.indexOf(listener);
      if (idx >= 0) listeners.splice(idx, 1);
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      const idx = onceListeners.indexOf(listener);
      if (idx >= 0) onceListeners.splice(idx, 1);
    }
    return this;
  }

  off(event: string, listener: NetEventListener): this {
    return this.removeListener(event, listener);
  }

  removeAllListeners(event?: string): this {
    if (event) {
      delete this._listeners[event];
      delete this._onceListeners[event];
    } else {
      this._listeners = {};
      this._onceListeners = {};
    }
    return this;
  }

  listeners(event: string): NetEventListener[] {
    return [...(this._listeners[event] ?? []), ...(this._onceListeners[event] ?? [])];
  }

  listenerCount(event: string): number {
    return (this._listeners[event]?.length ?? 0) + (this._onceListeners[event]?.length ?? 0);
  }

  setMaxListeners(_n: number): this { return this; }
  getMaxListeners(): number { return 10; }
  prependListener(event: string, listener: NetEventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].unshift(listener);
    return this;
  }
  prependOnceListener(event: string, listener: NetEventListener): this {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].unshift(listener);
    return this;
  }
  eventNames(): string[] {
    return [...new Set([...Object.keys(this._listeners), ...Object.keys(this._onceListeners)])];
  }
  rawListeners(event: string): NetEventListener[] {
    return this.listeners(event);
  }
  emit(event: string, ...args: unknown[]): boolean {
    return this._emitNet(event, ...args);
  }

  _emitNet(event: string, ...args: unknown[]): boolean {
    if (event === "data" && this._encoding && args[0] && Buffer.isBuffer(args[0])) {
      args[0] = (args[0] as Buffer).toString(this._encoding);
    }
    let handled = false;
    const listeners = this._listeners[event];
    if (listeners) {
      for (const fn of [...listeners]) {
        fn(...args);
        handled = true;
      }
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      const fns = [...onceListeners];
      this._onceListeners[event] = [];
      for (const fn of fns) {
        fn(...args);
        handled = true;
      }
    }
    return handled;
  }

  private async _waitForConnect(): Promise<void> {
    if (typeof _netSocketWaitConnectRaw === "undefined" || this._socketId === 0) {
      return;
    }
    try {
      const infoJson = await _netSocketWaitConnectRaw.apply(
        undefined,
        [this._socketId],
        { result: { promise: true } },
      );
      if (this.destroyed) {
        return;
      }
      this._applySocketInfo(parseNetSocketInfo(infoJson));
      this._connected = true;
      this.connecting = false;
      this._touchTimeout();
      this._emitNet("connect");
      this._emitNet("ready");
      if (!this._tlsUpgrading) {
        await this._pumpBridgeReads();
      }
    } catch (error) {
      if (this.destroyed) {
        return;
      }
      const err = error instanceof Error ? error : new Error(String(error));
      this._emitNet("error", err);
      this.destroy();
    }
  }

  private async _pumpBridgeReads(): Promise<void> {
    if (
      this._bridgeReadLoopRunning ||
      typeof _netSocketReadRaw === "undefined" ||
      this._socketId === 0
    ) {
      return;
    }
    this._bridgeReadLoopRunning = true;
    try {
      while (!this.destroyed) {
        const chunkBase64 = _netSocketReadRaw.applySync(undefined, [this._socketId]);
        if (this.destroyed) {
          return;
        }
        if (chunkBase64 === NET_BRIDGE_TIMEOUT_SENTINEL) {
          if (!this._refed) {
            return;
          }
          this._bridgeReadPollTimer = setTimeout(() => {
            this._bridgeReadPollTimer = null;
            void this._pumpBridgeReads();
          }, NET_BRIDGE_POLL_DELAY_MS);
          return;
        }
        if (chunkBase64 === null) {
          this.readable = false;
          this._readableState.endEmitted = true;
          this._emitNet("end");
          if (!this.destroyed) {
            unregisterNetSocket(this._socketId);
            this._emitNet("close");
          }
          return;
        }
        const payload = Buffer.from(chunkBase64, "base64");
        this.bytesRead += payload.length;
        this._touchTimeout();
        this._emitNet("data", payload);
      }
    } finally {
      this._bridgeReadLoopRunning = false;
    }
  }

  private _dispatchLoopbackHttpRequest(): void {
    if (!this._loopbackServer || this._loopbackDispatchRunning || this.destroyed) {
      return;
    }
    this._loopbackDispatchRunning = true;
    void this._processLoopbackHttpRequests().finally(() => {
      this._loopbackDispatchRunning = false;
    });
  }

  private async _processLoopbackHttpRequests(): Promise<void> {
    let closeAfterDrain = false;

    while (this._loopbackServer && !this.destroyed) {
      const parsed = parseLoopbackRequestBuffer(this._loopbackBuffer, this._loopbackServer);
      if (parsed.kind === "incomplete") {
        if (closeAfterDrain) {
          this._closeLoopbackReadable();
        }
        return;
      }

      if (parsed.kind === "bad-request") {
        this._pushLoopbackData(createBadRequestResponseBuffer());
        if (parsed.closeConnection) {
          this._closeLoopbackReadable();
        }
        this._loopbackBuffer = Buffer.alloc(0);
        return;
      }

      this._loopbackBuffer = this._loopbackBuffer.subarray(parsed.bytesConsumed);

      if (parsed.upgradeHead) {
        this._dispatchLoopbackUpgrade(parsed.request, parsed.upgradeHead);
        return;
      }

      const {
        responseJson,
      } = await dispatchLoopbackServerRequest(this._loopbackServer, parsed.request);
      const response = JSON.parse(responseJson) as SerializedServerResponse;
      const serialized = serializeLoopbackResponse(response, parsed.request, parsed.closeConnection);
      if (!closeAfterDrain && serialized.payload.length > 0) {
        this._pushLoopbackData(serialized.payload);
      }

      if (serialized.closeConnection) {
        closeAfterDrain = true;
        if (this._loopbackBuffer.length === 0) {
          this._closeLoopbackReadable();
          return;
        }
      }
    }
  }

  private _pushLoopbackData(data: Buffer): void {
    if (data.length === 0 || this._loopbackReadableEnded) {
      return;
    }
    const payload = Buffer.from(data);
    this._queueLoopbackEvent(() => {
      if (this.destroyed) {
        return;
      }
      this.bytesRead += payload.length;
      this._touchTimeout();
      this._emitNet("data", payload);
    });
  }

  private _closeLoopbackReadable(): void {
    if (this._loopbackReadableEnded) {
      return;
    }
    this._loopbackReadableEnded = true;
    this.readable = false;
    this.writable = false;
    this._readableState.endEmitted = true;
    this._clearTimeoutTimer();
    this._queueLoopbackEvent(() => {
      this._emitNet("end");
      this._emitNet("close");
    });
  }

  private _queueLoopbackEvent(callback: () => void): void {
    this._loopbackEventQueue = this._loopbackEventQueue.then(
      () => new Promise<void>((resolve) => {
        queueMicrotask(() => {
          try {
            callback();
          } finally {
            resolve();
          }
        });
      }),
    );
  }

  private _dispatchLoopbackUpgrade(
    request: SerializedServerRequest,
    head: Buffer,
  ): void {
    if (!this._loopbackServer) {
      return;
    }

    try {
      this._loopbackServer._emit(
        "upgrade",
        new ServerIncomingMessage(request),
        new DirectTunnelSocket({
          host: this.remoteAddress,
          port: this.remotePort,
        }),
        head,
      );
    } catch (error) {
      const rethrow =
        error instanceof Error
          ? error
          : new Error(String(error));
      let handled = false;
      let exitCodeFromHandler: number | null = null;
      if (typeof process !== "undefined" && typeof process.emit === "function") {
        const processEmitter = process as typeof process & {
          emit(event: string, ...args: unknown[]): boolean;
        };
        try {
          handled = processEmitter.emit("uncaughtException", rethrow, "uncaughtException");
        } catch (emitError) {
          if (
            emitError &&
            typeof emitError === "object" &&
            (emitError as { name?: string }).name === "ProcessExitError"
          ) {
            handled = true;
            const exitCode = Number((emitError as { code?: unknown }).code);
            exitCodeFromHandler = Number.isFinite(exitCode) ? exitCode : 0;
          } else {
            throw emitError;
          }
        }
      }
      if (handled) {
        if (exitCodeFromHandler !== null) {
          process.exitCode = exitCodeFromHandler;
        }
        this._loopbackServer?.close();
        this.destroy();
        return;
      }
      throw rethrow;
    }
  }

  // Upgrade this socket to TLS
  _upgradeTls(options?: SerializedTlsBridgeOptions): void {
    if (typeof _netSocketUpgradeTlsRaw === "undefined") {
      throw new Error("tls.connect is not supported in sandbox (bridge not available)");
    }
    this._tlsUpgrading = true;
    _netSocketUpgradeTlsRaw.applySync(undefined, [this._socketId, JSON.stringify(options ?? {})]);
  }

  _touchTimeout(): void {
    if (this._timeoutMs === 0 || this.destroyed) {
      return;
    }
    this._clearTimeoutTimer();
    this._timeoutTimer = setTimeout(() => {
      this._timeoutTimer = null;
      if (this.destroyed) {
        return;
      }
      this._emitNet("timeout");
    }, this._timeoutMs);
    if (!this._refed && typeof this._timeoutTimer.unref === "function") {
      this._timeoutTimer.unref();
    }
  }

  _clearTimeoutTimer(): void {
    if (this._timeoutTimer) {
      clearTimeout(this._timeoutTimer);
      this._timeoutTimer = null;
    }
  }
}

function netConnect(
  portOrOptions:
    | number
    | string
    | {
        host?: string;
        port?: number;
        path?: string;
        keepAlive?: unknown;
        keepAliveInitialDelay?: number;
      },
  hostOrCallback?: string | (() => void),
  callback?: () => void,
): NetSocket {
  const socket = new NetSocket();
  socket.connect(portOrOptions, hostOrCallback, callback);
  return socket;
}

type NetServerEventListener = (...args: unknown[]) => void;

class NetServer {
  private _listeners: Record<string, NetServerEventListener[]> = {};
  private _onceListeners: Record<string, NetServerEventListener[]> = {};
  private _serverId = 0;
  private _address: { address: string; family: string; port: number } | string | null = null;
  private _acceptLoopActive = false;
  private _acceptLoopRunning = false;
  private _acceptPollTimer: ReturnType<typeof setTimeout> | null = null;
  private _handleRefId: string | null = null;
  private _connections = new Set<NetSocket>();
  private _refed = true;
  listening = false;
  keepAlive = false;
  keepAliveInitialDelay = 0;
  allowHalfOpen = false;
  maxConnections?: number;
  _handle: {
    onconnection: (err: Error | null, clientHandle?: AcceptedNetClientHandle) => void;
  };

  constructor(
    optionsOrListener?: {
      allowHalfOpen?: boolean;
      keepAlive?: boolean;
      keepAliveInitialDelay?: number;
    } | NetServerEventListener,
    maybeListener?: NetServerEventListener,
  ) {
    if (typeof optionsOrListener === "function") {
      this.on("connection", optionsOrListener);
    } else {
      this.allowHalfOpen = optionsOrListener?.allowHalfOpen === true;
      this.keepAlive = optionsOrListener?.keepAlive === true;
      this.keepAliveInitialDelay = optionsOrListener?.keepAliveInitialDelay ?? 0;
      if (maybeListener) {
        this.on("connection", maybeListener);
      }
    }
    this._handle = {
      onconnection: (err: Error | null, clientHandle?: AcceptedNetClientHandle) => {
        if (err) {
          this._emit("error", err);
          return;
        }
        if (!clientHandle) {
          return;
        }
        if (
          typeof this.maxConnections === "number" &&
          this.maxConnections >= 0 &&
          this._connections.size >= this.maxConnections
        ) {
          this._emit("drop", {
            localAddress: clientHandle.info.localAddress,
            localPort: clientHandle.info.localPort,
            localFamily: clientHandle.info.localFamily,
            remoteAddress: clientHandle.info.remoteAddress,
            remotePort: clientHandle.info.remotePort,
            remoteFamily: clientHandle.info.remoteFamily,
          });
          _netSocketDestroyRaw?.applySync(undefined, [clientHandle.socketId]);
          return;
        }
        if (this.keepAlive) {
          clientHandle.setKeepAlive?.(true, this.keepAliveInitialDelay);
        }
        const socket = NetSocket.fromAcceptedHandle(clientHandle, {
          allowHalfOpen: this.allowHalfOpen,
        });
        socket.server = this;
        this._connections.add(socket);
        socket.once("close", () => {
          this._connections.delete(socket);
        });
        if (this.keepAlive) {
          socket._applyAcceptedKeepAlive(this.keepAliveInitialDelay);
        }
        this._emit("connection", socket);
      },
    };
  }

  listen(
    portOrOptions?: number | string | null | { port?: unknown; host?: unknown; backlog?: unknown; path?: unknown },
    hostOrCallback?: string | NetServerEventListener,
    backlogOrCallback?: number | NetServerEventListener,
    callback?: NetServerEventListener,
  ): this {
    if (typeof _netServerListenRaw === "undefined" || typeof _netServerAcceptRaw === "undefined") {
      throw new Error("net.createServer is not supported in sandbox");
    }

    const { port, host, path, backlog, readableAll, writableAll, callback: cb } = normalizeListenArgs(
      portOrOptions,
      hostOrCallback,
      backlogOrCallback,
      callback,
    );

    if (cb) {
      this.once("listening", cb);
    }

    try {
      const resultJson = _netServerListenRaw.applySyncPromise(
        undefined,
        [JSON.stringify({ port, host, path, backlog, readableAll, writableAll })],
      );
      const result = JSON.parse(resultJson) as {
        serverId: number;
        address: NetSocketInfo;
      };
      this._serverId = result.serverId;
      this._address = result.address.localPath
        ? result.address.localPath
        : {
            address: result.address.localAddress,
            family: result.address.localFamily,
            port: result.address.localPort,
          };
      this.listening = true;
      this._syncHandleRef();
      this._acceptLoopActive = true;
      queueMicrotask(() => {
        if (!this.listening || this._serverId === 0) {
          return;
        }
        this._emit("listening");
        void this._pumpAccepts();
      });
    } catch (error) {
      queueMicrotask(() => {
        this._emit("error", error);
      });
    }

    return this;
  }

  close(callback?: NetServerEventListener): this {
    if (callback) {
      this.once("close", callback);
    }
    if (!this.listening || typeof _netServerCloseRaw === "undefined") {
      queueMicrotask(() => {
        this._emit("close");
      });
      return this;
    }
    this.listening = false;
    this._acceptLoopActive = false;
    if (this._acceptPollTimer) {
      clearTimeout(this._acceptPollTimer);
      this._acceptPollTimer = null;
    }
    this._syncHandleRef();
    const serverId = this._serverId;
    this._serverId = 0;
    void (async () => {
      try {
        await _netServerCloseRaw.apply(undefined, [serverId], {
          result: { promise: true },
        });
      } finally {
        this._address = null;
        this._emit("close");
      }
    })();
    return this;
  }

  address(): { address: string; family: string; port: number } | string | null {
    return this._address;
  }

  getConnections(callback: (error: Error | null, count: number) => void): this {
    if (typeof callback !== "function") {
      throw createFunctionArgTypeError("callback", callback);
    }
    queueMicrotask(() => {
      callback(null, this._connections.size);
    });
    return this;
  }

  ref(): this {
    this._refed = true;
    this._syncHandleRef();
    if (this.listening && this._acceptLoopActive && !this._acceptLoopRunning) {
      void this._pumpAccepts();
    }
    return this;
  }

  unref(): this {
    this._refed = false;
    if (this._acceptPollTimer) {
      clearTimeout(this._acceptPollTimer);
      this._acceptPollTimer = null;
    }
    this._syncHandleRef();
    return this;
  }

  on(event: string, listener: NetServerEventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: NetServerEventListener): this {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    return this;
  }

  emit(event: string, ...args: unknown[]): boolean {
    return this._emit(event, ...args);
  }

  private _emit(event: string, ...args: unknown[]): boolean {
    let handled = false;
    const listeners = this._listeners[event];
    if (listeners) {
      for (const fn of [...listeners]) {
        fn(...args);
        handled = true;
      }
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      this._onceListeners[event] = [];
      for (const fn of [...onceListeners]) {
        fn(...args);
        handled = true;
      }
    }
    return handled;
  }

  private _syncHandleRef(): void {
    if (!this.listening || this._serverId === 0 || !this._refed) {
      if (this._handleRefId && typeof _unregisterHandle === "function") {
        _unregisterHandle(this._handleRefId);
      }
      this._handleRefId = null;
      return;
    }

    const nextHandleId = `${NET_SERVER_HANDLE_PREFIX}${this._serverId}`;
    if (this._handleRefId === nextHandleId) {
      return;
    }
    if (this._handleRefId && typeof _unregisterHandle === "function") {
      _unregisterHandle(this._handleRefId);
    }
    this._handleRefId = nextHandleId;
    if (typeof _registerHandle === "function") {
      _registerHandle(this._handleRefId, "net server");
    }
  }

  private async _pumpAccepts(): Promise<void> {
    if (typeof _netServerAcceptRaw === "undefined" || this._acceptLoopRunning) {
      return;
    }
    this._acceptLoopRunning = true;
    try {
      while (this._acceptLoopActive && this._serverId !== 0) {
        const payload = _netServerAcceptRaw.applySync(undefined, [this._serverId]);
        if (payload === NET_BRIDGE_TIMEOUT_SENTINEL) {
          if (!this._refed) {
            return;
          }
          this._acceptPollTimer = setTimeout(() => {
            this._acceptPollTimer = null;
            void this._pumpAccepts();
          }, NET_BRIDGE_POLL_DELAY_MS);
          return;
        }
        if (!payload) {
          return;
        }
        try {
          const accepted = JSON.parse(payload) as {
            socketId: number;
            info: NetSocketInfo;
          };
          const clientHandle = createAcceptedClientHandle(accepted.socketId, accepted.info);
          this._handle.onconnection(null, clientHandle);
        } catch (error) {
          this._emit("error", error);
        }
      }
    } finally {
      this._acceptLoopRunning = false;
    }
  }
}

function NetServerCallable(
  this: NetServer | undefined,
  optionsOrListener?: {
    allowHalfOpen?: boolean;
    keepAlive?: boolean;
    keepAliveInitialDelay?: number;
  } | NetServerEventListener,
  maybeListener?: NetServerEventListener,
): NetServer {
  return new NetServer(optionsOrListener, maybeListener);
}

function findLoopbackHttpServerByPort(port: number): Server | null {
  for (const server of serverInstances.values()) {
    if (!server.listening) {
      continue;
    }
    const address = server.address();
    if (address && typeof address === "object" && address.port === port) {
      return server;
    }
  }
  return null;
}

const netModule = {
  Socket: NetSocket,
  Server: NetServerCallable as unknown as typeof import("node:net").Server,
  connect: netConnect,
  createConnection: netConnect,
  createServer(
    optionsOrListener?: {
      allowHalfOpen?: boolean;
      keepAlive?: boolean;
      keepAliveInitialDelay?: number;
    } | NetServerEventListener,
    maybeListener?: NetServerEventListener,
  ): NetServer {
    return new NetServer(optionsOrListener, maybeListener);
  },
  isIP(input: string): number {
    return classifyIpAddress(input);
  },
  isIPv4(input: string): boolean { return classifyIpAddress(input) === 4; },
  isIPv6(input: string): boolean { return classifyIpAddress(input) === 6; },
};

// ===================================================================
// tls module — TLS socket support via upgrade bridge
// ===================================================================

type TlsConnectOptions = {
  host?: string;
  port?: number;
  socket?: NetSocket;
  rejectUnauthorized?: boolean;
  servername?: string;
  session?: Buffer | Uint8Array;
  ALPNProtocols?: string[];
  secureContext?: TlsSecureContextWrapper;
  key?: unknown;
  cert?: unknown;
  ca?: unknown;
  ciphers?: string;
  minVersion?: string;
  maxVersion?: string;
  passphrase?: string;
};

type TlsServerOptions = {
  allowHalfOpen?: boolean;
  keepAlive?: boolean;
  keepAliveInitialDelay?: number;
  rejectUnauthorized?: boolean;
  requestCert?: boolean;
  SNICallback?: (
    servername: string,
    callback: (error: Error | null, context: unknown) => void,
  ) => void;
  ALPNProtocols?: string[];
  ALPNCallback?: (info: {
    servername?: string;
    protocols: string[];
  }) => string | undefined;
  secureContext?: TlsSecureContextWrapper;
  key?: unknown;
  cert?: unknown;
  ca?: unknown;
  ciphers?: string;
  minVersion?: string;
  maxVersion?: string;
  passphrase?: string;
};

function createSecureContextWrapper(options?: Record<string, unknown>): TlsSecureContextWrapper {
  return {
    __secureExecTlsContext: buildSerializedTlsOptions(options),
    context: {},
  };
}

function tlsConnect(
  portOrOptions: number | TlsConnectOptions,
  hostOrCallback?: string | (() => void),
  callback?: () => void,
): NetSocket {
  let socket: NetSocket;
  let options: TlsConnectOptions = {};
  let cb: (() => void) | undefined;

  if (typeof portOrOptions === "object") {
    options = { ...portOrOptions };
    cb = typeof hostOrCallback === "function" ? hostOrCallback : callback;

    if (portOrOptions.socket) {
      // Upgrade existing socket to TLS
      socket = portOrOptions.socket;
    } else {
      // Create new TCP socket then upgrade
      socket = new NetSocket();
      socket.connect({ host: portOrOptions.host ?? "127.0.0.1", port: portOrOptions.port });
    }
  } else {
    const host = typeof hostOrCallback === "string" ? hostOrCallback : "127.0.0.1";
    cb = typeof hostOrCallback === "function" ? hostOrCallback : callback;
    options = { host };
    socket = new NetSocket();
    socket.connect(portOrOptions, host);
  }

  if (cb) socket.once("secureConnect", cb);

  const upgradeOptions = buildSerializedTlsOptions(
    options as unknown as Record<string, unknown>,
    {
      isServer: false,
      servername: options.servername ?? options.host ?? "127.0.0.1",
    },
  );
  socket.servername = upgradeOptions.servername;

  // If already connected, upgrade immediately; otherwise wait for connect
  if (socket._connected) {
    socket._upgradeTls(upgradeOptions);
  } else {
    socket.once("connect", () => {
      socket._upgradeTls(upgradeOptions);
    });
  }

  return socket;
}

function matchesServername(pattern: string, servername: string): boolean {
  if (!pattern.startsWith("*.")) {
    return pattern === servername;
  }
  const suffix = pattern.slice(1);
  if (!servername.endsWith(suffix)) {
    return false;
  }
  const prefix = servername.slice(0, -suffix.length);
  return prefix.length > 0 && !prefix.includes(".");
}

class TLSServer {
  private _listeners: Record<string, NetServerEventListener[]> = {};
  private _onceListeners: Record<string, NetServerEventListener[]> = {};
  private _server: NetServer;
  private _tlsOptions: SerializedTlsBridgeOptions;
  private _sniCallback?:
    | ((
        servername: string,
        callback: (error: Error | null, context: unknown) => void,
      ) => void)
    | undefined;
  private _alpnCallback?:
    | ((info: { servername?: string; protocols: string[] }) => string | undefined)
    | undefined;
  private _contexts: Array<{
    servername: string;
    context: TlsSecureContextWrapper;
  }> = [];

  constructor(
    optionsOrListener?: TlsServerOptions | NetServerEventListener,
    maybeListener?: NetServerEventListener,
  ) {
    const options =
      typeof optionsOrListener === "function" || optionsOrListener === undefined
        ? undefined
        : optionsOrListener;
    const listener =
      typeof optionsOrListener === "function" ? optionsOrListener : maybeListener;

    if (options?.ALPNCallback && options?.ALPNProtocols) {
      const error = new Error(
        "The ALPNCallback and ALPNProtocols TLS options are mutually exclusive",
      ) as Error & { code?: string };
      error.code = "ERR_TLS_ALPN_CALLBACK_WITH_PROTOCOLS";
      throw error;
    }

    this._tlsOptions = buildSerializedTlsOptions(
      options as unknown as Record<string, unknown> | undefined,
      { isServer: true },
    );
    this._sniCallback = options?.SNICallback;
    this._alpnCallback = options?.ALPNCallback;

    this._server = new NetServer(
      options
        ? {
            allowHalfOpen: options.allowHalfOpen,
            keepAlive: options.keepAlive,
            keepAliveInitialDelay: options.keepAliveInitialDelay,
          }
        : undefined,
      ((socket: unknown) => {
        const tlsSocket = socket as NetSocket;
        tlsSocket.server = this as unknown as NetServer;
        void this._handleSecureSocket(tlsSocket);
      }) as NetServerEventListener,
    );

    if (listener) {
      this.on("secureConnection", listener);
    }

    this._server.on("listening", (...args) => this._emit("listening", ...args));
    this._server.on("close", (...args) => this._emit("close", ...args));
    this._server.on("error", (...args) => this._emit("error", ...args));
    this._server.on("drop", (...args) => this._emit("drop", ...args));
  }

  listen(
    portOrOptions?: number | string | null | { port?: unknown; host?: unknown; backlog?: unknown; path?: unknown },
    hostOrCallback?: string | NetServerEventListener,
    backlogOrCallback?: number | NetServerEventListener,
    callback?: NetServerEventListener,
  ): this {
    this._server.listen(portOrOptions, hostOrCallback, backlogOrCallback, callback);
    return this;
  }

  close(callback?: NetServerEventListener): this {
    if (callback) {
      this.once("close", callback);
    }
    this._server.close();
    return this;
  }

  address(): { address: string; family: string; port: number } | string | null {
    return this._server.address();
  }

  getConnections(callback: (error: Error | null, count: number) => void): this {
    this._server.getConnections(callback);
    return this;
  }

  ref(): this {
    this._server.ref();
    return this;
  }

  unref(): this {
    this._server.unref();
    return this;
  }

  addContext(servername: string, context: unknown): this {
    const wrapper = isTlsSecureContextWrapper(context)
      ? context
      : createSecureContextWrapper(
          context && typeof context === "object"
            ? context as Record<string, unknown>
            : undefined,
        );
    this._contexts.push({ servername, context: wrapper });
    return this;
  }

  on(event: string, listener: NetServerEventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  once(event: string, listener: NetServerEventListener): this {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    return this;
  }

  emit(event: string, ...args: unknown[]): boolean {
    return this._emit(event, ...args);
  }

  private _emit(event: string, ...args: unknown[]): boolean {
    let handled = false;
    const listeners = this._listeners[event];
    if (listeners) {
      for (const fn of [...listeners]) {
        fn(...args);
        handled = true;
      }
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      this._onceListeners[event] = [];
      for (const fn of [...onceListeners]) {
        fn(...args);
        handled = true;
      }
    }
    return handled;
  }

  private async _handleSecureSocket(socket: NetSocket): Promise<void> {
    const clientHello = this._getClientHello(socket);
    const requestedServername = clientHello?.servername;
    if (requestedServername) {
      socket.servername = requestedServername;
    }

    try {
      const upgradeOptions = await this._resolveTlsOptions(
        requestedServername,
        clientHello?.ALPNProtocols ?? [],
      );
      if (!upgradeOptions) {
        this._emitTlsClientError(socket, "Invalid SNI context");
        return;
      }

      socket._upgradeTls(upgradeOptions);
      socket.once("secure", () => {
        this._emit("secureConnection", socket);
        this._emit("connection", socket);
      });
      socket.on("error", (error: unknown) => {
        this._emit("tlsClientError", error, socket);
      });
    } catch (error) {
      const err = error instanceof Error ? error : new Error(String(error));
      this._emitTlsClientError(socket, err.message, err);
      if ((err as { uncaught?: boolean }).uncaught) {
        (process as typeof process & {
          emit?: (event: string, ...args: unknown[]) => boolean;
        }).emit?.("uncaughtException", err, "uncaughtException");
      }
    }
  }

  private _getClientHello(socket: NetSocket): SerializedTlsClientHello | null {
    if (typeof _netSocketGetTlsClientHelloRaw === "undefined") {
      return null;
    }
    const socketId = (socket as unknown as { _socketId?: number })._socketId;
    if (typeof socketId !== "number" || socketId === 0) {
      return null;
    }
    return parseTlsClientHello(
      _netSocketGetTlsClientHelloRaw.applySync(undefined, [socketId]),
    );
  }

  private async _resolveTlsOptions(
    servername: string | undefined,
    clientProtocols: string[],
  ): Promise<SerializedTlsBridgeOptions | null> {
    let selectedContext: TlsSecureContextWrapper | null = null;
    let invalidContext = false;

    if (servername && this._sniCallback) {
      selectedContext = await new Promise<TlsSecureContextWrapper | null>((resolve, reject) => {
        this._sniCallback?.(servername, (error, context) => {
          if (error) {
            reject(error);
            return;
          }
          if (context == null) {
            resolve(null);
            return;
          }
          if (isTlsSecureContextWrapper(context)) {
            resolve(context);
            return;
          }
          if (context && typeof context === "object" && Object.keys(context as object).length > 0) {
            resolve(createSecureContextWrapper(context as Record<string, unknown>));
            return;
          }
          invalidContext = true;
          resolve(null);
        });
      });
      if (invalidContext) {
        return null;
      }
    } else if (servername) {
      selectedContext = this._findContext(servername);
    }

    const resolvedOptions: SerializedTlsBridgeOptions = {
      ...this._tlsOptions,
      ...(selectedContext?.__secureExecTlsContext ?? {}),
      isServer: true,
    };

    if (this._alpnCallback) {
      const selectedProtocol = this._alpnCallback({
        servername,
        protocols: clientProtocols,
      });
      if (selectedProtocol === undefined) {
        const error = new Error("ALPN callback rejected the client protocol list") as Error & {
          code?: string;
        };
        error.code = "ERR_SSL_TLSV1_ALERT_NO_APPLICATION_PROTOCOL";
        throw error;
      }
      if (!clientProtocols.includes(selectedProtocol)) {
        const error = new Error(
          "The ALPNCallback callback returned an invalid protocol",
        ) as Error & { code?: string; uncaught?: boolean };
        error.code = "ERR_TLS_ALPN_CALLBACK_INVALID_RESULT";
        error.uncaught = true;
        throw error;
      }
      resolvedOptions.ALPNProtocols = [selectedProtocol];
    }

    return resolvedOptions;
  }

  private _findContext(servername: string): TlsSecureContextWrapper | null {
    for (let index = this._contexts.length - 1; index >= 0; index -= 1) {
      const entry = this._contexts[index];
      if (matchesServername(entry.servername, servername)) {
        return entry.context;
      }
    }
    return null;
  }

  private _emitTlsClientError(
    socket: NetSocket,
    message: string,
    existingError?: Error,
  ): void {
    const error = existingError ?? new Error(message);
    socket.servername ??= this._getClientHello(socket)?.servername;
    this._emit("tlsClientError", error, socket);
    socket.destroy();
  }
}

function TLSServerCallable(
  this: TLSServer | undefined,
  optionsOrListener?: TlsServerOptions | NetServerEventListener,
  maybeListener?: NetServerEventListener,
): TLSServer {
  return new TLSServer(optionsOrListener, maybeListener);
}

const tlsModule = {
  connect: tlsConnect,
  TLSSocket: NetSocket, // Alias — TLSSocket is just a NetSocket after upgrade
  Server: TLSServerCallable as unknown as typeof import("node:tls").Server,
  createServer(
    optionsOrListener?: TlsServerOptions | NetServerEventListener,
    maybeListener?: NetServerEventListener,
  ): TLSServer {
    return new TLSServer(optionsOrListener, maybeListener);
  },
  createSecureContext(options?: Record<string, unknown>): TlsSecureContextWrapper {
    return createSecureContextWrapper(options);
  },
  getCiphers(): string[] {
    if (typeof _tlsGetCiphersRaw === "undefined") {
      throw new Error("tls.getCiphers is not supported in sandbox");
    }
    try {
      return JSON.parse(_tlsGetCiphersRaw.applySync(undefined, [])) as string[];
    } catch {
      return [];
    }
  },
  DEFAULT_MIN_VERSION: "TLSv1.2",
  DEFAULT_MAX_VERSION: "TLSv1.3",
};

type DgramEventListener = (...args: unknown[]) => void;
type DgramSocketType = "udp4" | "udp6";
type DgramRemoteInfo = {
  address: string;
  family: string;
  port: number;
  size: number;
};

type DgramSocketAddress = {
  address: string;
  family: string;
  port: number;
};

type SerializedDgramMessage = {
  data: string;
  rinfo: DgramRemoteInfo;
};

const DGRAM_HANDLE_PREFIX = "dgram-socket:";

function createBadDgramSocketTypeError(): TypeError & { code: string } {
  return createTypeErrorWithCode(
    "Bad socket type specified. Valid types are: udp4, udp6",
    "ERR_SOCKET_BAD_TYPE",
  );
}

function createDgramAlreadyBoundError(): Error & { code: string } {
  const error = new Error("Socket is already bound") as Error & { code: string };
  error.code = "ERR_SOCKET_ALREADY_BOUND";
  return error;
}

function createDgramAddressError(): Error {
  return new Error("getsockname EBADF");
}

function createDgramArgTypeError(
  argumentName: string,
  expectedType: string,
  value: unknown,
): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The "${argumentName}" argument must be of type ${expectedType}. Received ${formatReceivedType(value)}`,
    "ERR_INVALID_ARG_TYPE",
  );
}

function createDgramMissingArgError(argumentName: string): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The "${argumentName}" argument must be specified`,
    "ERR_MISSING_ARGS",
  );
}

function createDgramNotRunningError(): Error & { code: string } {
  return createErrorWithCode("Not running", "ERR_SOCKET_DGRAM_NOT_RUNNING");
}

function getDgramErrno(code: "EBADF" | "EINVAL" | "EADDRNOTAVAIL" | "ENOPROTOOPT"): number {
  switch (code) {
    case "EBADF":
      return -9;
    case "EINVAL":
      return -22;
    case "EADDRNOTAVAIL":
      return -99;
    case "ENOPROTOOPT":
      return -92;
  }
}

function createDgramSyscallError(
  syscall: string,
  code: "EBADF" | "EINVAL" | "EADDRNOTAVAIL" | "ENOPROTOOPT",
): Error & { code: string; errno: number; syscall: string } {
  const error = new Error(`${syscall} ${code}`) as Error & {
    code: string;
    errno: number;
    syscall: string;
  };
  error.code = code;
  error.errno = getDgramErrno(code);
  error.syscall = syscall;
  return error;
}

function createDgramTtlArgTypeError(value: unknown): TypeError & { code: string } {
  return createTypeErrorWithCode(
    `The "ttl" argument must be of type number. Received ${formatReceivedType(value)}`,
    "ERR_INVALID_ARG_TYPE",
  );
}

function createDgramBufferSizeTypeError(): TypeError & { code: string } {
  return createTypeErrorWithCode(
    "Buffer size must be a positive integer",
    "ERR_SOCKET_BAD_BUFFER_SIZE",
  );
}

function createDgramBufferSizeSystemError(
  which: "recv" | "send",
  code: "EBADF" | "EINVAL",
): Error & {
  code: string;
  info: { errno: number; code: string; message: string; syscall: string };
  errno: number;
  syscall: string;
} {
  const syscall = `uv_${which}_buffer_size`;
  const info = {
    errno: code === "EBADF" ? -9 : -22,
    code,
    message: code === "EBADF" ? "bad file descriptor" : "invalid argument",
    syscall,
  };
  const error = new Error(
    `Could not get or set buffer size: ${syscall} returned ${code} (${info.message})`,
  ) as Error & {
    code: string;
    info: { errno: number; code: string; message: string; syscall: string };
    errno: number;
    syscall: string;
  };
  error.name = "SystemError [ERR_SOCKET_BUFFER_SIZE]";
  error.code = "ERR_SOCKET_BUFFER_SIZE";
  error.info = info;
  let errno = info.errno;
  let syscallValue = syscall;
  Object.defineProperty(error, "errno", {
    enumerable: true,
    configurable: true,
    get() {
      return errno;
    },
    set(value: number) {
      errno = value;
    },
  });
  Object.defineProperty(error, "syscall", {
    enumerable: true,
    configurable: true,
    get() {
      return syscallValue;
    },
    set(value: string) {
      syscallValue = value;
    },
  });
  return error;
}

function getPlatformDgramBufferSize(size: number): number {
  if (size <= 0) {
    return size;
  }
  return process.platform === "linux" ? size * 2 : size;
}

function normalizeDgramTtlValue(
  value: unknown,
  syscall: "setTTL" | "setMulticastTTL",
): number {
  if (typeof value !== "number") {
    throw createDgramTtlArgTypeError(value);
  }
  if (!Number.isInteger(value) || value <= 0 || value >= 256) {
    throw createDgramSyscallError(syscall, "EINVAL");
  }
  return value;
}

function isIPv4MulticastAddress(address: string): boolean {
  if (!isIPv4String(address)) {
    return false;
  }
  const first = Number(address.split(".")[0]);
  return first >= 224 && first <= 239;
}

function isIPv4UnicastAddress(address: string): boolean {
  return isIPv4String(address) && !isIPv4MulticastAddress(address) && address !== "255.255.255.255";
}

function isIPv6MulticastAddress(address: string): boolean {
  const zoneIndex = address.indexOf("%");
  const normalized = zoneIndex === -1 ? address : address.slice(0, zoneIndex);
  return isIPv6String(address) && normalized.toLowerCase().startsWith("ff");
}

function validateDgramMulticastAddress(
  type: DgramSocketType,
  syscall: "addMembership" | "dropMembership" | "addSourceSpecificMembership" | "dropSourceSpecificMembership",
  address: unknown,
): string {
  if (typeof address !== "string") {
    throw createDgramArgTypeError(
      syscall === "addSourceSpecificMembership" || syscall === "dropSourceSpecificMembership"
        ? "groupAddress"
        : "multicastAddress",
      "string",
      address,
    );
  }
  const isValid = type === "udp6" ? isIPv6MulticastAddress(address) : isIPv4MulticastAddress(address);
  if (!isValid) {
    throw createDgramSyscallError(syscall, "EINVAL");
  }
  return address;
}

function validateDgramSourceAddress(
  type: DgramSocketType,
  syscall: "addSourceSpecificMembership" | "dropSourceSpecificMembership",
  address: unknown,
): string {
  if (typeof address !== "string") {
    throw createDgramArgTypeError("sourceAddress", "string", address);
  }
  const isValid = type === "udp6" ? isIPv6String(address) && !isIPv6MulticastAddress(address) : isIPv4UnicastAddress(address);
  if (!isValid) {
    throw createDgramSyscallError(syscall, "EINVAL");
  }
  return address;
}

function normalizeDgramSocketType(value: unknown): DgramSocketType {
  if (value === "udp4" || value === "udp6") {
    return value;
  }
  throw createBadDgramSocketTypeError();
}

function normalizeDgramSocketOptions(
  options: unknown,
): { type: DgramSocketType; recvBufferSize?: number; sendBufferSize?: number } {
  if (typeof options === "string") {
    return { type: normalizeDgramSocketType(options) };
  }
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw createBadDgramSocketTypeError();
  }
  const typedOptions = options as {
    type?: unknown;
    recvBufferSize?: unknown;
    sendBufferSize?: unknown;
  };
  const result: { type: DgramSocketType; recvBufferSize?: number; sendBufferSize?: number } = {
    type: normalizeDgramSocketType(typedOptions.type),
  };
  if (typedOptions.recvBufferSize !== undefined) {
    if (typeof typedOptions.recvBufferSize !== "number") {
      throw createInvalidArgTypeError(
        "options.recvBufferSize",
        "number",
        typedOptions.recvBufferSize,
      );
    }
    result.recvBufferSize = typedOptions.recvBufferSize;
  }
  if (typedOptions.sendBufferSize !== undefined) {
    if (typeof typedOptions.sendBufferSize !== "number") {
      throw createInvalidArgTypeError(
        "options.sendBufferSize",
        "number",
        typedOptions.sendBufferSize,
      );
    }
    result.sendBufferSize = typedOptions.sendBufferSize;
  }
  return result;
}

function normalizeDgramAddressValue(
  address: unknown,
  type: DgramSocketType,
  defaultAddress: string,
): string {
  if (address === undefined || address === null || address === "") {
    return defaultAddress;
  }
  if (typeof address !== "string") {
    throw createDgramArgTypeError("address", "string", address);
  }
  if (address === "localhost") {
    return type === "udp6" ? "::1" : "127.0.0.1";
  }
  return address;
}

function normalizeDgramPortValue(port: unknown): number {
  if (typeof port !== "number") {
    throw createDgramArgTypeError("port", "number", port);
  }
  if (!isValidTcpPort(port)) {
    throw createSocketBadPortError(port);
  }
  return port;
}

function createDgramMessageBuffer(value: unknown): Buffer {
  if (typeof value === "string") {
    return Buffer.from(value);
  }
  if (Buffer.isBuffer(value)) {
    return Buffer.from(value);
  }
  if (ArrayBuffer.isView(value)) {
    return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  }
  throw createDgramArgTypeError("msg", "string or Buffer or Uint8Array or DataView", value);
}

function createDgramMessageListBuffer(value: unknown): Buffer {
  if (Array.isArray(value)) {
    return Buffer.concat(value.map((entry) => createDgramMessageBuffer(entry)));
  }
  return createDgramMessageBuffer(value);
}

function normalizeDgramBindArgs(
  args: unknown[],
  type: DgramSocketType,
): { port: number; address: string; callback?: () => void } {
  let port: unknown;
  let address: unknown;
  let callback: unknown;

  if (typeof args[0] === "function") {
    callback = args[0];
  } else if (args[0] && typeof args[0] === "object" && !Array.isArray(args[0])) {
    const options = args[0] as { port?: unknown; address?: unknown };
    port = options.port;
    address = options.address;
    callback = args[1];
  } else {
    port = args[0];
    if (typeof args[1] === "function") {
      callback = args[1];
    } else {
      address = args[1];
      callback = args[2];
    }
  }

  if (callback !== undefined && typeof callback !== "function") {
    throw createFunctionArgTypeError("callback", callback);
  }

  return {
    port: port === undefined ? 0 : normalizeDgramPortValue(port),
    address: normalizeDgramAddressValue(
      address,
      type,
      type === "udp6" ? "::" : "0.0.0.0",
    ),
    callback: callback as (() => void) | undefined,
  };
}

function normalizeDgramSendArgs(
  args: unknown[],
  type: DgramSocketType,
): { data: Buffer; port: number; address: string; callback?: (err: Error | null, bytes?: number) => void } {
  if (args.length === 0) {
    throw createDgramArgTypeError("msg", "string or Buffer or Uint8Array or DataView", undefined);
  }
  const message = args[0];
  const hasOffsetLength =
    typeof args[1] === "number" &&
    typeof args[2] === "number" &&
    args.length >= 4;

  if (hasOffsetLength) {
    const source = createDgramMessageBuffer(message);
    const offset = args[1] as number;
    const length = args[2] as number;
    const callback = typeof args[4] === "function" ? args[4] : args[5];
    if (callback !== undefined && typeof callback !== "function") {
      throw createFunctionArgTypeError("callback", callback);
    }
    return {
      data: Buffer.from(source.subarray(offset, offset + length)),
      port: normalizeDgramPortValue(args[3]),
      address: normalizeDgramAddressValue(
        typeof args[4] === "function" ? undefined : args[4],
        type,
        type === "udp6" ? "::1" : "127.0.0.1",
      ),
      callback: callback as ((err: Error | null, bytes?: number) => void) | undefined,
    };
  }

  const callback = typeof args[2] === "function" ? args[2] : args[3];
  if (callback !== undefined && typeof callback !== "function") {
    throw createFunctionArgTypeError("callback", callback);
  }
  return {
    data: createDgramMessageListBuffer(message),
    port: normalizeDgramPortValue(args[1]),
    address: normalizeDgramAddressValue(
      typeof args[2] === "function" ? undefined : args[2],
      type,
      type === "udp6" ? "::1" : "127.0.0.1",
    ),
    callback: callback as ((err: Error | null, bytes?: number) => void) | undefined,
  };
}

class DgramSocket {
  private readonly _type: DgramSocketType;
  private readonly _socketId: number;
  private _listeners: Record<string, DgramEventListener[]> = {};
  private _onceListeners: Record<string, DgramEventListener[]> = {};
  private _bindPromise: Promise<void> | null = null;
  private _receiveLoopRunning = false;
  private _receivePollTimer: ReturnType<typeof setTimeout> | null = null;
  private _refed = true;
  private _closed = false;
  private _bound = false;
  private _handleRefId: string | null = null;
  private _recvBufferSize?: number;
  private _sendBufferSize?: number;
  private _memberships = new Set<string>();
  private _multicastInterface?: string;
  private _broadcast = false;
  private _multicastLoopback = 1;
  private _multicastTtl = 1;
  private _ttl = 64;

  constructor(
    optionsOrType: unknown,
    listener?: DgramEventListener,
  ) {
    if (typeof _dgramSocketCreateRaw === "undefined") {
      throw new Error("dgram.createSocket is not supported in sandbox");
    }
    const options = normalizeDgramSocketOptions(optionsOrType);
    this._type = options.type;
    this._socketId = _dgramSocketCreateRaw.applySync(undefined, [this._type]);
    if (listener) {
      this.on("message", listener);
    }
    if (options.recvBufferSize !== undefined) {
      this._setBufferSize("recv", options.recvBufferSize, false);
    }
    if (options.sendBufferSize !== undefined) {
      this._setBufferSize("send", options.sendBufferSize, false);
    }
  }

  bind(...args: unknown[]): this {
    const { port, address, callback } = normalizeDgramBindArgs(args, this._type);
    void this._bindInternal(port, address, callback);
    return this;
  }

  send(...args: unknown[]): void {
    const { data, port, address, callback } = normalizeDgramSendArgs(args, this._type);
    void this._sendInternal(data, port, address, callback);
  }

  sendto(...args: unknown[]): void {
    this.send(...args);
  }

  address(): DgramSocketAddress {
    if (typeof _dgramSocketAddressRaw === "undefined") {
      throw createDgramAddressError();
    }
    try {
      return JSON.parse(
        _dgramSocketAddressRaw.applySync(undefined, [this._socketId]),
      ) as DgramSocketAddress;
    } catch {
      throw createDgramAddressError();
    }
  }

  close(callback?: () => void): this {
    if (callback !== undefined && typeof callback !== "function") {
      throw createFunctionArgTypeError("callback", callback);
    }
    if (callback) {
      this.once("close", callback);
    }
    if (this._closed) {
      return this;
    }
    this._closed = true;
    this._bound = false;
    this._clearReceivePollTimer();
    this._syncHandleRef();
    if (typeof _dgramSocketCloseRaw === "undefined") {
      queueMicrotask(() => {
        this._emit("close");
      });
      return this;
    }
    try {
      _dgramSocketCloseRaw.applySyncPromise(undefined, [this._socketId]);
    } finally {
      queueMicrotask(() => {
        this._emit("close");
      });
    }
    return this;
  }

  ref(): this {
    this._refed = true;
    this._syncHandleRef();
    if (this._receivePollTimer && typeof this._receivePollTimer.ref === "function") {
      this._receivePollTimer.ref();
    }
    if (this._bound && !this._closed && !this._receiveLoopRunning) {
      void this._pumpMessages();
    }
    return this;
  }

  unref(): this {
    this._refed = false;
    this._syncHandleRef();
    if (this._receivePollTimer && typeof this._receivePollTimer.unref === "function") {
      this._receivePollTimer.unref();
    }
    return this;
  }

  setRecvBufferSize(size: number): void {
    this._setBufferSize("recv", size);
  }

  setSendBufferSize(size: number): void {
    this._setBufferSize("send", size);
  }

  getRecvBufferSize(): number {
    return this._getBufferSize("recv");
  }

  getSendBufferSize(): number {
    return this._getBufferSize("send");
  }

  setBroadcast(flag: unknown): void {
    this._ensureBoundForSocketOption("setBroadcast");
    this._broadcast = Boolean(flag);
  }

  setTTL(ttl: unknown): number {
    this._ensureBoundForSocketOption("setTTL");
    this._ttl = normalizeDgramTtlValue(ttl, "setTTL");
    return this._ttl;
  }

  setMulticastTTL(ttl: unknown): number {
    this._ensureBoundForSocketOption("setMulticastTTL");
    this._multicastTtl = normalizeDgramTtlValue(ttl, "setMulticastTTL");
    return this._multicastTtl;
  }

  setMulticastLoopback(flag: unknown): number {
    this._ensureBoundForSocketOption("setMulticastLoopback");
    this._multicastLoopback = Number(flag);
    return this._multicastLoopback;
  }

  addMembership(multicastAddress?: unknown, multicastInterface?: unknown): void {
    if (multicastAddress === undefined) {
      throw createDgramMissingArgError("multicastAddress");
    }
    if (this._closed) {
      throw createDgramNotRunningError();
    }
    const groupAddress = validateDgramMulticastAddress(
      this._type,
      "addMembership",
      multicastAddress,
    );
    if (multicastInterface !== undefined && typeof multicastInterface !== "string") {
      throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
    }
    this._memberships.add(`${groupAddress}|${multicastInterface ?? ""}`);
  }

  dropMembership(multicastAddress?: unknown, multicastInterface?: unknown): void {
    if (multicastAddress === undefined) {
      throw createDgramMissingArgError("multicastAddress");
    }
    if (this._closed) {
      throw createDgramNotRunningError();
    }
    const groupAddress = validateDgramMulticastAddress(
      this._type,
      "dropMembership",
      multicastAddress,
    );
    if (multicastInterface !== undefined && typeof multicastInterface !== "string") {
      throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
    }
    const membershipKey = `${groupAddress}|${multicastInterface ?? ""}`;
    if (!this._memberships.has(membershipKey)) {
      throw createDgramSyscallError("dropMembership", "EADDRNOTAVAIL");
    }
    this._memberships.delete(membershipKey);
  }

  addSourceSpecificMembership(
    sourceAddress?: unknown,
    groupAddress?: unknown,
    multicastInterface?: unknown,
  ): void {
    if (this._closed) {
      throw createDgramNotRunningError();
    }
    if (typeof sourceAddress !== "string") {
      throw createDgramArgTypeError("sourceAddress", "string", sourceAddress);
    }
    if (typeof groupAddress !== "string") {
      throw createDgramArgTypeError("groupAddress", "string", groupAddress);
    }
    const validatedSource = validateDgramSourceAddress(
      this._type,
      "addSourceSpecificMembership",
      sourceAddress,
    );
    const validatedGroup = validateDgramMulticastAddress(
      this._type,
      "addSourceSpecificMembership",
      groupAddress,
    );
    if (multicastInterface !== undefined && typeof multicastInterface !== "string") {
      throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
    }
    this._memberships.add(`${validatedSource}>${validatedGroup}|${multicastInterface ?? ""}`);
  }

  dropSourceSpecificMembership(
    sourceAddress?: unknown,
    groupAddress?: unknown,
    multicastInterface?: unknown,
  ): void {
    if (this._closed) {
      throw createDgramNotRunningError();
    }
    if (typeof sourceAddress !== "string") {
      throw createDgramArgTypeError("sourceAddress", "string", sourceAddress);
    }
    if (typeof groupAddress !== "string") {
      throw createDgramArgTypeError("groupAddress", "string", groupAddress);
    }
    const validatedSource = validateDgramSourceAddress(
      this._type,
      "dropSourceSpecificMembership",
      sourceAddress,
    );
    const validatedGroup = validateDgramMulticastAddress(
      this._type,
      "dropSourceSpecificMembership",
      groupAddress,
    );
    if (multicastInterface !== undefined && typeof multicastInterface !== "string") {
      throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
    }
    const membershipKey = `${validatedSource}>${validatedGroup}|${multicastInterface ?? ""}`;
    if (!this._memberships.has(membershipKey)) {
      throw createDgramSyscallError("dropSourceSpecificMembership", "EADDRNOTAVAIL");
    }
    this._memberships.delete(membershipKey);
  }

  setMulticastInterface(interfaceAddress: unknown): void {
    if (typeof interfaceAddress !== "string") {
      throw createDgramArgTypeError("interfaceAddress", "string", interfaceAddress);
    }
    if (this._closed) {
      throw createDgramNotRunningError();
    }
    this._ensureBoundForSocketOption("setMulticastInterface");
    if (this._type === "udp4") {
      if (interfaceAddress === "0.0.0.0") {
        this._multicastInterface = interfaceAddress;
        return;
      }
      if (!isIPv4String(interfaceAddress)) {
        throw createDgramSyscallError("setMulticastInterface", "ENOPROTOOPT");
      }
      if (!isIPv4UnicastAddress(interfaceAddress)) {
        throw createDgramSyscallError("setMulticastInterface", "EADDRNOTAVAIL");
      }
      this._multicastInterface = interfaceAddress;
      return;
    }
    if (interfaceAddress === "" || interfaceAddress === "undefined" || !isIPv6String(interfaceAddress)) {
      throw createDgramSyscallError("setMulticastInterface", "EINVAL");
    }
    this._multicastInterface = interfaceAddress;
  }

  on(event: string, listener: DgramEventListener): this {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }

  addListener(event: string, listener: DgramEventListener): this {
    return this.on(event, listener);
  }

  once(event: string, listener: DgramEventListener): this {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    return this;
  }

  removeListener(event: string, listener: DgramEventListener): this {
    const listeners = this._listeners[event];
    if (listeners) {
      const index = listeners.indexOf(listener);
      if (index >= 0) listeners.splice(index, 1);
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      const index = onceListeners.indexOf(listener);
      if (index >= 0) onceListeners.splice(index, 1);
    }
    return this;
  }

  off(event: string, listener: DgramEventListener): this {
    return this.removeListener(event, listener);
  }

  emit(event: string, ...args: unknown[]): boolean {
    return this._emit(event, ...args);
  }

  private async _bindInternal(
    port: number,
    address: string,
    callback?: () => void,
  ): Promise<void> {
    if (this._closed) {
      return;
    }
    if (this._bound || this._bindPromise) {
      throw createDgramAlreadyBoundError();
    }
    if (typeof _dgramSocketBindRaw === "undefined") {
      throw new Error("dgram.bind is not supported in sandbox");
    }

    this._bindPromise = (async () => {
      try {
        const resultJson = _dgramSocketBindRaw.applySyncPromise(undefined, [
          this._socketId,
          JSON.stringify({ port, address }),
        ]);
        JSON.parse(resultJson) as DgramSocketAddress;
        this._bound = true;
        this._applyInitialBufferSizes();
        this._syncHandleRef();
        queueMicrotask(() => {
          if (this._closed) {
            return;
          }
          this._emit("listening");
          callback?.call(this);
          void this._pumpMessages();
        });
      } catch (error) {
        queueMicrotask(() => {
          this._emit("error", error);
        });
        throw error;
      } finally {
        this._bindPromise = null;
      }
    })();

    return this._bindPromise;
  }

  private async _ensureBound(): Promise<void> {
    if (this._bound) {
      return;
    }
    if (this._bindPromise) {
      await this._bindPromise;
      return;
    }
    await this._bindInternal(0, this._type === "udp6" ? "::" : "0.0.0.0");
  }

  private async _sendInternal(
    data: Buffer,
    port: number,
    address: string,
    callback?: (err: Error | null, bytes?: number) => void,
  ): Promise<void> {
    try {
      await this._ensureBound();
      if (this._closed || typeof _dgramSocketSendRaw === "undefined") {
        return;
      }
      const bytes = _dgramSocketSendRaw.applySyncPromise(undefined, [
        this._socketId,
        JSON.stringify({
          data: data.toString("base64"),
          port,
          address,
        }),
      ]);
      if (callback) {
        queueMicrotask(() => {
          callback(null, bytes);
        });
      }
    } catch (error) {
      if (callback) {
        queueMicrotask(() => {
          callback(error as Error);
        });
        return;
      }
      queueMicrotask(() => {
        this._emit("error", error);
      });
    }
  }

  private async _pumpMessages(): Promise<void> {
    if (this._receiveLoopRunning || this._closed || !this._bound) {
      return;
    }
    if (typeof _dgramSocketRecvRaw === "undefined") {
      return;
    }

    this._receiveLoopRunning = true;
    try {
      while (!this._closed && this._bound) {
        const payload = _dgramSocketRecvRaw.applySync(undefined, [this._socketId]);
        if (payload === NET_BRIDGE_TIMEOUT_SENTINEL) {
          this._receivePollTimer = setTimeout(() => {
            this._receivePollTimer = null;
            void this._pumpMessages();
          }, NET_BRIDGE_POLL_DELAY_MS);
          if (!this._refed && typeof this._receivePollTimer.unref === "function") {
            this._receivePollTimer.unref();
          }
          return;
        }
        if (!payload) {
          return;
        }
        const message = JSON.parse(payload) as SerializedDgramMessage;
        this._emit(
          "message",
          Buffer.from(message.data, "base64"),
          message.rinfo,
        );
      }
    } catch (error) {
      this._emit("error", error);
    } finally {
      this._receiveLoopRunning = false;
    }
  }

  private _clearReceivePollTimer(): void {
    if (this._receivePollTimer) {
      clearTimeout(this._receivePollTimer);
      this._receivePollTimer = null;
    }
  }

  private _ensureBoundForSocketOption(
    syscall: "setBroadcast" | "setTTL" | "setMulticastTTL" | "setMulticastLoopback" | "setMulticastInterface",
  ): void {
    if (!this._bound || this._closed) {
      throw createDgramSyscallError(syscall, "EBADF");
    }
  }

  private _setBufferSize(which: "recv" | "send", size: number, requireRunning = true): void {
    if (!Number.isInteger(size) || size <= 0 || !Number.isFinite(size)) {
      throw createDgramBufferSizeTypeError();
    }
    if (size > 0x7fffffff) {
      throw createDgramBufferSizeSystemError(which, "EINVAL");
    }
    if (requireRunning && (!this._bound || this._closed)) {
      throw createDgramBufferSizeSystemError(which, "EBADF");
    }
    if (typeof _dgramSocketSetBufferSizeRaw !== "undefined" && this._bound && !this._closed) {
      _dgramSocketSetBufferSizeRaw.applySync(undefined, [this._socketId, which, size]);
    }
    if (which === "recv") {
      this._recvBufferSize = size;
      return;
    }
    this._sendBufferSize = size;
  }

  private _getBufferSize(which: "recv" | "send"): number {
    if (!this._bound || this._closed) {
      throw createDgramBufferSizeSystemError(which, "EBADF");
    }
    const fallback = which === "recv" ? this._recvBufferSize ?? 0 : this._sendBufferSize ?? 0;
    if (typeof _dgramSocketGetBufferSizeRaw === "undefined") {
      return getPlatformDgramBufferSize(fallback);
    }
    const rawSize = _dgramSocketGetBufferSizeRaw.applySync(undefined, [this._socketId, which]);
    return getPlatformDgramBufferSize(rawSize > 0 ? rawSize : fallback);
  }

  private _applyInitialBufferSizes(): void {
    if (this._recvBufferSize !== undefined) {
      this._setBufferSize("recv", this._recvBufferSize);
    }
    if (this._sendBufferSize !== undefined) {
      this._setBufferSize("send", this._sendBufferSize);
    }
  }

  private _syncHandleRef(): void {
    if (!this._bound || this._closed || !this._refed) {
      if (this._handleRefId && typeof _unregisterHandle === "function") {
        _unregisterHandle(this._handleRefId);
      }
      this._handleRefId = null;
      return;
    }

    const nextHandleId = `${DGRAM_HANDLE_PREFIX}${this._socketId}`;
    if (this._handleRefId === nextHandleId) {
      return;
    }
    if (this._handleRefId && typeof _unregisterHandle === "function") {
      _unregisterHandle(this._handleRefId);
    }
    this._handleRefId = nextHandleId;
    if (typeof _registerHandle === "function") {
      _registerHandle(this._handleRefId, "dgram socket");
    }
  }

  private _emit(event: string, ...args: unknown[]): boolean {
    let handled = false;
    const listeners = this._listeners[event];
    if (listeners) {
      for (const listener of [...listeners]) {
        listener(...args);
        handled = true;
      }
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      this._onceListeners[event] = [];
      for (const listener of [...onceListeners]) {
        listener(...args);
        handled = true;
      }
    }
    return handled;
  }
}

const dgramModule = {
  Socket: DgramSocket as unknown as typeof nodeDgram.Socket,
  createSocket(
    optionsOrType: unknown,
    callback?: DgramEventListener,
  ): DgramSocket {
    return new DgramSocket(optionsOrType, callback);
  },
};

exposeCustomGlobal("_netModule", netModule);
exposeCustomGlobal("_tlsModule", tlsModule);
exposeCustomGlobal("_dgramModule", dgramModule);

export default {
  fetch,
  Headers,
  Request,
  Response,
  dns,
  http,
  https,
  http2,
  IncomingMessage,
  ClientRequest,
  net: netModule,
  tls: tlsModule,
  dgram: dgramModule,
};
