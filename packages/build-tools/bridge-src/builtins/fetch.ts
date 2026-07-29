import { getAgentOsUndiciDispatcher, undiciFetch } from "./undici.js";
import { exposeCustomGlobal, exposeInstallCompatibleHardenedGlobal } from "../global-exposure.js";
import { undiciFormDataModule, undiciHeadersModule, undiciRequestModule, undiciResponseModule } from "../prelude.js";
import { isFlatHeaderList, onUpgradeSocketEnd } from "./http.js";
import { resolveObjectURL } from "./whatwg-url.js";

// npm 11 requests full registry metadata while resolving manifests. Large,
// long-lived packages such as drizzle-orm can exceed 50 MiB even though their
// install tarball is much smaller. Keep buffering bounded while allowing those
// package-manager responses to pass through the Node compatibility layer.
var MAX_HTTP_BODY_BYTES = 128 * 1024 * 1024;

var MAX_HTTP_REQUEST_HEADER_BYTES = 64 * 1024;

var MAX_HTTP_REQUEST_HEADERS = 2e3;

var _fetchHandleCounter = 0;

var UndiciHeaders = undiciHeadersModule?.Headers ?? undiciHeadersModule?.default ?? undiciHeadersModule;

var UndiciRequest = undiciRequestModule?.Request ?? undiciRequestModule?.default ?? undiciRequestModule;

var UndiciResponse = undiciResponseModule?.Response ?? undiciResponseModule?.default ?? undiciResponseModule;

var UndiciFormData = undiciFormDataModule?.FormData ?? undiciFormDataModule?.default ?? undiciFormDataModule;

var MAX_FORM_DATA_ENTRIES = 1024;
var MAX_FORM_DATA_VALUE_BYTES = 64 * 1024 * 1024;
var kBoundedFormData = Symbol.for("agentOS.boundedFormData");

function formDataEntryCount(formData) {
  let count = 0;
  for (const _entry of formData) {
    count += 1;
    if (count > MAX_FORM_DATA_ENTRIES) break;
  }
  return count;
}

function assertFormDataValueBound(value) {
  const bytes = typeof Blob === "function" && value instanceof Blob
    ? value.size
    : new TextEncoder().encode(`${value}`).byteLength;
  if (bytes > MAX_FORM_DATA_VALUE_BYTES) {
    const error = new RangeError(`FormData value byte limit ${MAX_FORM_DATA_VALUE_BYTES} exceeded; this runtime limit cannot be raised by guest code`);
    error.code = "ERR_FORM_DATA_VALUE_SIZE_LIMIT";
    throw error;
  }
}

function installBoundedFormData(FormDataCtor) {
  if (typeof FormDataCtor !== "function" || FormDataCtor.prototype[kBoundedFormData]) return;
  const originalAppend = FormDataCtor.prototype.append;
  const originalSet = FormDataCtor.prototype.set;
  const originalHas = FormDataCtor.prototype.has;
  FormDataCtor.prototype.append = function append(name, value, filename) {
    if (formDataEntryCount(this) >= MAX_FORM_DATA_ENTRIES) {
      const error = new RangeError(`FormData entry limit ${MAX_FORM_DATA_ENTRIES} exceeded; this runtime limit cannot be raised by guest code`);
      error.code = "ERR_FORM_DATA_ENTRIES_LIMIT";
      throw error;
    }
    assertFormDataValueBound(value);
    return arguments.length >= 3
      ? originalAppend.call(this, name, value, filename)
      : originalAppend.call(this, name, value);
  };
  FormDataCtor.prototype.set = function set(name, value, filename) {
    if (!originalHas.call(this, name) && formDataEntryCount(this) >= MAX_FORM_DATA_ENTRIES) {
      const error = new RangeError(`FormData entry limit ${MAX_FORM_DATA_ENTRIES} exceeded; this runtime limit cannot be raised by guest code`);
      error.code = "ERR_FORM_DATA_ENTRIES_LIMIT";
      throw error;
    }
    assertFormDataValueBound(value);
    return arguments.length >= 3
      ? originalSet.call(this, name, value, filename)
      : originalSet.call(this, name, value);
  };
  Object.defineProperty(FormDataCtor.prototype, kBoundedFormData, { value: true });
}

installBoundedFormData(UndiciFormData);

function serializeFetchHeaders(headers) {
  if (!headers) {
    return {};
  }
  if (headers instanceof Headers) {
    return Object.fromEntries(headers.entries());
  }
  if (typeof UndiciHeaders === "function" && headers instanceof UndiciHeaders) {
    return Object.fromEntries(headers.entries());
  }
  if (isFlatHeaderList(headers)) {
    const normalized = {};
    for (let index = 0; index < headers.length; index += 2) {
      const key = headers[index];
      const value = headers[index + 1];
      if (key !== void 0 && value !== void 0) {
        normalized[key] = value;
      }
    }
    return normalized;
  }
  if (typeof headers.entries === "function") {
    return Object.fromEntries(headers.entries());
  }
  if (typeof headers[Symbol.iterator] === "function") {
    return Object.fromEntries(headers);
  }
  return Object.fromEntries(new Headers(headers).entries());
}

function createFetchHeaders(headers) {
  return new Headers(serializeFetchHeaders(headers));
}

function normalizeFetchRequestInit(options = {}) {
  const normalized = { ...options };
  // Some bundled Node SDKs pass node-fetch style `agent` options into fetch().
  // Undici doesn't accept that field, and the default global dispatcher already
  // routes through the agentos virtual network stack.
  if (Object.prototype.hasOwnProperty.call(normalized, "agent")) {
    delete normalized.agent;
  }
  if (Object.prototype.hasOwnProperty.call(normalized, "headers")) {
    normalized.headers = serializeFetchHeaders(normalized.headers);
  }
  if (
    normalized.body != null &&
    normalized.duplex == null &&
    String(normalized.method ?? "GET").toUpperCase() !== "GET" &&
    String(normalized.method ?? "GET").toUpperCase() !== "HEAD"
  ) {
    normalized.duplex = "half";
  }
  return normalized;
}

function ensureFetchAcceptEncoding(options) {
  const headers = serializeFetchHeaders(options?.headers);
  const hasAcceptEncoding = Object.keys(headers).some(
    (key) => key.toLowerCase() === "accept-encoding"
  );
  if (!hasAcceptEncoding) {
    headers["accept-encoding"] = "gzip, deflate";
  }
  return { ...(options || {}), headers };
}

function blobUrlResponse(url, options) {
  const parsed = new URL(url);
  if (parsed.search) throw new TypeError("fetch failed");
  parsed.hash = "";
  const method = String(options.method ?? "GET").toUpperCase();
  const blob = resolveObjectURL(parsed.href);
  if (method !== "GET" || !(blob instanceof Blob)) throw new TypeError("fetch failed");
  if (options.signal?.aborted) throw options.signal.reason;

  const headers = serializeFetchHeaders(options.headers);
  const rangeEntry = Object.entries(headers).find(([name]) => name.toLowerCase() === "range");
  let body = blob;
  let status = 200;
  let statusText = "OK";
  const responseHeaders = {
    "content-length": String(blob.size),
    ...(blob.type ? { "content-type": blob.type } : {})
  };
  if (rangeEntry) {
    const match = /^bytes=(\d+)-(\d*)$/.exec(String(rangeEntry[1]).trim());
    if (!match) throw new TypeError("fetch failed");
    const start = Number(match[1]);
    const requestedEnd = match[2] ? Number(match[2]) : blob.size - 1;
    if (start >= blob.size || requestedEnd < start) throw new TypeError("fetch failed");
    const end = Math.min(requestedEnd, blob.size - 1);
    body = blob.slice(start, end + 1, blob.type);
    status = 206;
    statusText = "Partial Content";
    responseHeaders["content-length"] = String(end - start + 1);
    responseHeaders["content-range"] = `bytes ${start}-${end}/${blob.size}`;
  }
  const response = new UndiciResponse(body, { status, statusText, headers: responseHeaders });
  Object.defineProperties(response, {
    url: { configurable: true, value: parsed.href },
    type: { configurable: true, value: "basic" }
  });
  return response;
}

async function fetch(input, options = {}) {
  if (typeof undiciFetch !== "function") {
    throw new Error("fetch requires undici to be configured");
  }
  let resolvedInput = input;
  let normalizedOptions = options;
  if (input instanceof Request || typeof UndiciRequest === "function" && input instanceof UndiciRequest) {
    resolvedInput = input.url;
    normalizedOptions = {
      method: input.method,
      headers: serializeFetchHeaders(input.headers),
      body: input.body,
      signal: input.signal,
      ...options
    };
  }
  normalizedOptions = normalizeFetchRequestInit(normalizedOptions);
  normalizedOptions = ensureFetchAcceptEncoding(normalizedOptions);
  const requestLabel = typeof resolvedInput === "string" ? resolvedInput : resolvedInput?.url ? String(resolvedInput.url) : String(resolvedInput);
  if (requestLabel.startsWith("blob:nodedata:")) {
    return blobUrlResponse(requestLabel, normalizedOptions);
  }
  const handleId = typeof _registerHandle === "function" ? `fetch:${++_fetchHandleCounter}` : null;
  if (handleId) {
    _registerHandle?.(handleId, `fetch ${requestLabel}`);
  }
  // Shared bounded dispatcher (see undici.ts): keepalive pooling across fetch()
  // calls. Per-call dispatchers (the 4f470c61 workaround for pooled clients
  // going stale against released sockets) are no longer needed now that
  // host->guest socket event push keeps pooled connections live.
  const fetchDispatcher = normalizedOptions.dispatcher == null && typeof getAgentOsUndiciDispatcher === "function" ? getAgentOsUndiciDispatcher() : null;
  try {
    return await undiciFetch(
      resolvedInput,
      fetchDispatcher ? { ...normalizedOptions, dispatcher: fetchDispatcher } : normalizedOptions
    );
  } finally {
    if (handleId) {
      _unregisterHandle?.(handleId);
    }
  }
}

var Headers = class _Headers {
  _headers = {};
  constructor(init) {
    if (init && init !== null) {
      if (init instanceof _Headers) {
        this._headers = { ...init._headers };
      } else if (Array.isArray(init)) {
        init.forEach(([key, value]) => {
          this._headers[key.toLowerCase()] = value;
        });
      } else if (typeof init === "object") {
        Object.entries(init).forEach(([key, value]) => {
          this._headers[key.toLowerCase()] = value;
        });
      }
    }
  }
  get(name) {
    return this._headers[name.toLowerCase()] || null;
  }
  set(name, value) {
    this._headers[name.toLowerCase()] = value;
  }
  has(name) {
    return name.toLowerCase() in this._headers;
  }
  delete(name) {
    delete this._headers[name.toLowerCase()];
  }
  entries() {
    return Object.entries(this._headers)[Symbol.iterator]();
  }
  [Symbol.iterator]() {
    return this.entries();
  }
  keys() {
    return Object.keys(this._headers)[Symbol.iterator]();
  }
  values() {
    return Object.values(this._headers)[Symbol.iterator]();
  }
  append(name, value) {
    const key = name.toLowerCase();
    if (key in this._headers) {
      this._headers[key] = this._headers[key] + ", " + value;
    } else {
      this._headers[key] = value;
    }
  }
  forEach(callback) {
    Object.entries(this._headers).forEach(([k, v]) => callback(v, k, this));
  }
};

var Request = class _Request {
  url;
  method;
  headers;
  body;
  mode;
  credentials;
  cache;
  redirect;
  referrer;
  integrity;
  constructor(input, init = {}) {
    this.url = typeof input === "string" ? input : input.url;
    this.method = init.method || (typeof input !== "string" ? input.method : void 0) || "GET";
    this.headers = createFetchHeaders(
      init.headers || (typeof input !== "string" ? input.headers : void 0)
    );
    this.body = init.body || null;
    this.mode = init.mode || "cors";
    this.credentials = init.credentials || "same-origin";
    this.cache = init.cache || "default";
    this.redirect = init.redirect || "follow";
    this.referrer = init.referrer || "about:client";
    this.integrity = init.integrity || "";
  }
  clone() {
    return new _Request(this.url, this);
  }
};

var Response = class _Response {
  _body;
  status;
  statusText;
  headers;
  ok;
  type;
  url;
  redirected;
  constructor(body, init = {}) {
    this._body = body || null;
    this.status = init.status || 200;
    this.statusText = init.statusText || "OK";
    this.headers = new Headers(init.headers);
    this.ok = this.status >= 200 && this.status < 300;
    this.type = "default";
    this.url = "";
    this.redirected = false;
  }
  async text() {
    return String(this._body || "");
  }
  async json() {
    return JSON.parse(this._body || "{}");
  }
  get body() {
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
          }
        };
      }
    };
  }
  clone() {
    return new _Response(this._body, { status: this.status, statusText: this.statusText });
  }
  static error() {
    return new _Response(null, { status: 0, statusText: "" });
  }
  static redirect(url, status = 302) {
    return new _Response(null, { status, headers: { Location: url } });
  }
};

exposeCustomGlobal("_upgradeSocketEnd", onUpgradeSocketEnd);

exposeInstallCompatibleHardenedGlobal("fetch", fetch);

exposeInstallCompatibleHardenedGlobal("Headers", UndiciHeaders);

exposeInstallCompatibleHardenedGlobal("Request", UndiciRequest);

exposeInstallCompatibleHardenedGlobal("Response", UndiciResponse);

var Blob = globalThis.Blob;
exposeInstallCompatibleHardenedGlobal("Blob", Blob);

var File = globalThis.File;
exposeInstallCompatibleHardenedGlobal("File", File);

var FormData = UndiciFormData;
exposeInstallCompatibleHardenedGlobal("FormData", FormData);
export { Blob, File, FormData, Headers, MAX_FORM_DATA_ENTRIES, MAX_FORM_DATA_VALUE_BYTES, MAX_HTTP_BODY_BYTES, MAX_HTTP_REQUEST_HEADERS, MAX_HTTP_REQUEST_HEADER_BYTES, Request, Response, UndiciFormData, UndiciHeaders, UndiciRequest, UndiciResponse, _fetchHandleCounter, blobUrlResponse, createFetchHeaders, ensureFetchAcceptEncoding, fetch, formDataEntryCount, installBoundedFormData, normalizeFetchRequestInit, serializeFetchHeaders };
