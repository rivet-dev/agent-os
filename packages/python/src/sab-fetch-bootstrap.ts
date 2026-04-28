/**
 * SharedArrayBuffer + side-worker bootstrap for synchronous fetch from
 * Python inside Pyodide.
 *
 * # Why this exists
 *
 * Pyodide's main module is built with Asyncify, so Python code there can
 * synchronously await JS Promises (urllib, asyncio, etc.). But .so files
 * dlopen'd as side modules (like the DuckDB Python wheel) can't suspend
 * — they don't share the main module's Asyncify state.
 *
 * Some libraries — most notably DuckDB's parquet reader — expect a
 * synchronous filesystem and call back into Python from C++ frames.
 * When DuckDB calls into a Python `fsspec` filesystem and that Python
 * tries to await a JS Promise, the call stack has C++ → JS → Python →
 * JS frames. Even with JSPI enabled, the JS frames between the wasm
 * suspension point and the wasm side module's call site can't be
 * unwound. Result: `SuspendError: trying to suspend JS frames`.
 *
 * `Atomics.wait` is a different mechanism: it's a synchronous JS-level
 * block that doesn't need to unwind any frames. The Pyodide worker
 * thread blocks; a side worker does the actual async fetch and signals
 * via `Atomics.notify`. No Asyncify, no JSPI, just synchronous JS.
 *
 * # SAB layout
 *
 * 16 bytes of header followed by variable-length payload:
 *
 * | offset | type | meaning                                  |
 * |-------:|------|------------------------------------------|
 * |      0 | i32  | state: 0=pending, 1=success, 2=error     |
 * |      4 | i32  | response status code                     |
 * |      8 | i32  | response body byte length                |
 * |     12 | i32  | response headers utf-8 byte length       |
 * |     16 | bytes| response body                            |
 * | 16+bl  | bytes| response headers ("key: val\n" lines)    |
 *
 * Default SAB size is 64 MB — enough for parquet footers + a few
 * row-group column chunks. For very large bodies, send the request
 * with a `Range` header and read in chunks; the side worker raises
 * RangeError if the response exceeds capacity so callers can retry.
 *
 * # Permissions caveat
 *
 * The side worker calls Node's native `fetch()` directly — it does
 * NOT route through agent-os's NetworkAdapter / permission gate. For
 * deployments that require permission enforcement on every fetch,
 * pass a `MessagePort` from the parent into the side worker so each
 * request round-trips through `callHost("networkFetch", ...)`. Tracked
 * as a TODO; the playground's Pi VM has full network access by config.
 */

/** Pre-built worker source — inlined in WORKER_SOURCE so the side
 *  worker can be spawned via `eval: true` Workers.
 *
 *  Includes inline SigV4 signing for `s3://bucket/key` URLs. The side
 *  worker reads AWS-style credentials from process.env and builds a
 *  signed `https://<endpoint>/<bucket>/<key>` request. Endpoint comes
 *  from BUCKET_ENDPOINT or ENDPOINT (set by the host's session env).
 *  Anonymous endpoints (public S3, MinIO with anonymous access) work
 *  without credentials — we skip signing if either key is empty. */
const SAB_SIDE_WORKER_JS = String.raw`
const { parentPort } = require("node:worker_threads");
const { createHmac, createHash } = require("node:crypto");

// Tiny SigV4 signer. Inlined to avoid a dep on @aws-sdk/signature-v4
// (which would balloon the wheel preload bundle by several MB).
function sha256Hex(buf) {
  return createHash("sha256").update(buf).digest("hex");
}
function hmac(key, data) {
  return createHmac("sha256", key).update(data).digest();
}
function signSigV4(method, url, body, accessKey, secretKey, region, service) {
  const u = new URL(url);
  const now = new Date();
  const amzDate = now.toISOString().replace(/[:-]|\.\d{3}/g, "");
  const dateStamp = amzDate.slice(0, 8);
  const payloadHash = sha256Hex(body || "");
  const canonicalUri = u.pathname || "/";
  const canonicalQuery = [...u.searchParams.entries()]
    .sort()
    .map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v))
    .join("&");
  const headers = {
    host: u.host,
    "x-amz-date": amzDate,
    "x-amz-content-sha256": payloadHash,
  };
  const sortedHeaderKeys = Object.keys(headers).sort();
  const canonicalHeaders = sortedHeaderKeys.map((k) => k + ":" + headers[k] + "\n").join("");
  const signedHeaders = sortedHeaderKeys.join(";");
  const canonicalRequest = [
    method,
    canonicalUri,
    canonicalQuery,
    canonicalHeaders,
    signedHeaders,
    payloadHash,
  ].join("\n");
  const credentialScope = dateStamp + "/" + region + "/" + service + "/aws4_request";
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    amzDate,
    credentialScope,
    sha256Hex(canonicalRequest),
  ].join("\n");
  const kDate = hmac("AWS4" + secretKey, dateStamp);
  const kRegion = hmac(kDate, region);
  const kService = hmac(kRegion, service);
  const kSigning = hmac(kService, "aws4_request");
  const signature = createHmac("sha256", kSigning)
    .update(stringToSign)
    .digest("hex");
  const authorization =
    "AWS4-HMAC-SHA256 Credential=" + accessKey + "/" + credentialScope +
    ", SignedHeaders=" + signedHeaders + ", Signature=" + signature;
  return { ...headers, Authorization: authorization };
}

// Translate s3://bucket/key to a signed https://endpoint/bucket/key
// (path-style addressing — works with both AWS S3 and MinIO without
// DNS shenanigans). Returns { url, headers } ready to pass to fetch.
function rewriteS3Url(s3url, init) {
  if (!s3url.startsWith("s3://")) return null;
  const stripped = s3url.slice("s3://".length);
  const slash = stripped.indexOf("/");
  if (slash < 0) return null;
  const bucket = stripped.slice(0, slash);
  const key = stripped.slice(slash + 1);
  const endpoint =
    process.env.BUCKET_ENDPOINT || process.env.ENDPOINT || "https://s3.amazonaws.com";
  const region = process.env.BUCKET_REGION || process.env.REGION || "us-east-1";
  const access = process.env.BUCKET_ACCESS_KEY_ID || process.env.ACCESS_KEY_ID || "";
  const secret = process.env.BUCKET_SECRET_ACCESS_KEY || process.env.SECRET_ACCESS_KEY || "";

  // Path-style URL: <endpoint>/<bucket>/<key>. AWS prefers virtual-host
  // style for new buckets but path-style still works and matches MinIO.
  const ep = endpoint.replace(/\/$/, "");
  const url = ep + "/" + encodeURIComponent(bucket) + "/" + key.split("/").map(encodeURIComponent).join("/");

  const method = (init && init.method) || "GET";
  const userHeaders = (init && init.headers) || {};
  const body = (init && init.body) || "";

  let signedHeaders = {};
  if (access && secret) {
    signedHeaders = signSigV4(method, url, body, access, secret, region, "s3");
  }
  return {
    url,
    init: {
      ...init,
      method,
      headers: { ...userHeaders, ...signedHeaders },
    },
  };
}

parentPort.on("message", async (msg) => {
  const { url, init, sab } = msg;
  const i32 = new Int32Array(sab, 0, 4);
  const fullView = new Uint8Array(sab);
  try {
    let actualUrl = url;
    let actualInit = init || {};
    const rewrite = rewriteS3Url(url, init);
    if (rewrite) {
      actualUrl = rewrite.url;
      actualInit = rewrite.init;
    }
    const r = await fetch(actualUrl, actualInit);
    const buf = await r.arrayBuffer();
    const bodyBytes = new Uint8Array(buf);
    const cap = sab.byteLength - 16;
    const enc = new TextEncoder();
    const headerLines = [];
    r.headers.forEach((v, k) => headerLines.push(k + ": " + v));
    const hdrBytes = enc.encode(headerLines.join("\n"));
    if (bodyBytes.byteLength + hdrBytes.byteLength > cap) {
      throw new Error(
        "response too large for SAB (" + bodyBytes.byteLength + " bytes body + " +
        hdrBytes.byteLength + " bytes headers > " + cap + " bytes capacity); " +
        "use a Range header to read in chunks"
      );
    }
    i32[1] = r.status;
    i32[2] = bodyBytes.byteLength;
    i32[3] = hdrBytes.byteLength;
    fullView.set(bodyBytes, 16);
    fullView.set(hdrBytes, 16 + bodyBytes.byteLength);
    Atomics.store(i32, 0, 1);
    Atomics.notify(i32, 0);
  } catch (err) {
    const enc = new TextEncoder();
    const errBytes = enc.encode(String((err && err.message) || err));
    i32[1] = 0;
    i32[2] = 0;
    i32[3] = errBytes.byteLength;
    fullView.set(errBytes, 16);
    Atomics.store(i32, 0, 2);
    Atomics.notify(i32, 0);
  }
});
`;

/**
 * Worker-side JS that defines `startSabFetch()` and a helper to register
 * the `_pyodide_httpfs_host` module on a pyodide instance. This string is
 * inlined into the Pyodide worker's WORKER_SOURCE template (driver.ts).
 *
 * Exposes (inside the Pyodide worker):
 *   startSabFetch()                                — returns sabFetch fn
 *   registerSabFetchModule(pyodide, sabFetch)      — wires the JS module
 *
 * Both are no-ops in unsupported environments (no SharedArrayBuffer
 * available); the registered module's fetch() throws a clear error so
 * Python code can fall back to async secure_exec.fetch.
 */
export const WORKER_SAB_FETCH_JS = String.raw`
const SAB_FETCH_SIZE = 64 * 1024 * 1024;
const SAB_SIDE_WORKER_SRC = ${JSON.stringify(SAB_SIDE_WORKER_JS)};

function startSabFetch() {
  if (typeof SharedArrayBuffer === "undefined") {
    return null;
  }
  const { Worker } = require("node:worker_threads");
  const worker = new Worker(SAB_SIDE_WORKER_SRC, { eval: true });
  worker.unref();
  return function sabFetch(url, init) {
    const sab = new SharedArrayBuffer(SAB_FETCH_SIZE);
    const i32 = new Int32Array(sab, 0, 4);
    i32[0] = 0;
    worker.postMessage({ url, init: init || {}, sab });
    Atomics.wait(i32, 0, 0);
    const state = i32[0];
    const status = i32[1];
    const bodyLen = i32[2];
    const hdrLen = i32[3];
    const fullView = new Uint8Array(sab);
    const body = fullView.slice(16, 16 + bodyLen);
    const hdrText = new TextDecoder().decode(
      fullView.slice(16 + bodyLen, 16 + bodyLen + hdrLen),
    );
    const headers = {};
    for (const line of hdrText.split("\n")) {
      const c = line.indexOf(":");
      if (c < 0) continue;
      headers[line.slice(0, c).trim().toLowerCase()] = line.slice(c + 1).trim();
    }
    if (state === 2) return { error: hdrText, status: 0, headers: {}, body };
    return { error: null, status, headers, body };
  };
}

function registerSabFetchModule(pyodide, sabFetch) {
  pyodide.registerJsModule("_pyodide_httpfs_host", {
    fetch: (url, initJson) => {
      if (!sabFetch) {
        throw new Error(
          "_pyodide_httpfs_host.fetch unavailable: SharedArrayBuffer not supported in this runtime"
        );
      }
      const init = typeof initJson === "string" ? JSON.parse(initJson) : initJson;
      return sabFetch(url, init);
    },
  });
}
`;
