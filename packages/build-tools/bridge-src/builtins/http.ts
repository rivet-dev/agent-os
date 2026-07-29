import { UndiciClient, undiciRequest } from "./undici.js";
import { dispatchCustomEmitterListeners } from "./process.js";
import { setImmediate } from "./timers.js";
import { exposeCustomGlobal } from "../global-exposure.js";
import { dns } from "./dns.js";
import { Headers, MAX_HTTP_BODY_BYTES, MAX_HTTP_REQUEST_HEADERS, MAX_HTTP_REQUEST_HEADER_BYTES, Request, Response } from "./fetch.js";
import { http2Servers, onHttp2Dispatch, pendingHttp2CompatRequests } from "./http2.js";
import { NetServer, NetSocket, netConnect } from "./net.js";
import { TLSSocket, tlsConnect } from "./tls.js";

function createConnResetError(message = "socket hang up") {
  const error = new Error(message);
  error.code = "ECONNRESET";
  return error;
}

function createAbortError2() {
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  return error;
}

var IncomingMessage = class {
  headers;
  rawHeaders;
  trailers;
  rawTrailers;
  httpVersion;
  httpVersionMajor;
  httpVersionMinor;
  method;
  url;
  statusCode;
  statusMessage;
  _body;
  _isBinary;
  _listeners;
  complete;
  aborted;
  socket;
  _bodyConsumed;
  _ended;
  _flowing;
  readable;
  readableEnded;
  readableFlowing;
  destroyed;
  _encoding;
  _closeEmitted;
  _readableScheduled;
  constructor(response) {
    const normalizedHeaders = {};
    if (Array.isArray(response?.headers)) {
      response.headers.forEach(([key, value]) => {
        appendNormalizedHeader(normalizedHeaders, key.toLowerCase(), value);
      });
    } else if (response?.headers) {
      Object.entries(response.headers).forEach(([key, value]) => {
        normalizedHeaders[key] = Array.isArray(value) ? [...value] : value;
      });
    }
    this.rawHeaders = Array.isArray(response?.rawHeaders) ? [...response.rawHeaders] : [];
    if (this.rawHeaders.length > 0) {
      this.headers = {};
      for (let index = 0; index < this.rawHeaders.length; index += 2) {
        const key = this.rawHeaders[index];
        const value = this.rawHeaders[index + 1];
        if (key !== void 0 && value !== void 0) {
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
    const bodyEncodingHeader = this.headers["x-body-encoding"];
    const bodyEncoding = response?.bodyEncoding || (Array.isArray(bodyEncodingHeader) ? bodyEncodingHeader[0] : bodyEncodingHeader);
    if (bodyEncoding === "base64" && response?.body && typeof Buffer !== "undefined") {
      this._body = Buffer.from(response.body, "base64").toString("binary");
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
    this._readableScheduled = false;
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    if (event === "data" && !this._bodyConsumed) {
      this._flowing = true;
      this.readableFlowing = true;
      Promise.resolve().then(() => {
        if (!this._bodyConsumed && this._flowing) {
          this._bodyConsumed = true;
          if (this._body && this._body.length > 0) {
            let buf;
            if (typeof Buffer !== "undefined") {
              buf = this._isBinary ? Buffer.from(this._body, "binary") : Buffer.from(this._body);
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
    if (event === "readable" && !this._bodyConsumed && !this._readableScheduled) {
      this._flowing = false;
      this.readableFlowing = false;
      this._readableScheduled = true;
      queueMicrotask(() => {
        this._readableScheduled = false;
        if (!this._bodyConsumed && !this.destroyed) this.emit("readable");
      });
    }
    return this;
  }
  addListener(event, listener) {
    return this.on(event, listener);
  }
  prependListener(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].unshift(listener);
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    wrapper._originalListener = listener;
    wrapper.listener = listener;
    return this.on(event, wrapper);
  }
  prependOnceListener(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    wrapper._originalListener = listener;
    wrapper.listener = listener;
    return this.prependListener(event, wrapper);
  }
  off(event, listener) {
    if (this._listeners[event]) {
      const idx = this._listeners[event].findIndex(
        (fn) => fn === listener || fn._originalListener === listener
      );
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  removeAllListeners(event) {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }
  listeners(event) {
    return (this._listeners[event] || []).map(
      (listener) => listener.listener || listener
    );
  }
  listenerCount(event) {
    return this._listeners[event]?.length || 0;
  }
  emit(event, ...args) {
    return dispatchCustomEmitterListeners(this, this._listeners[event], args);
  }
  setEncoding(encoding) {
    this._encoding = encoding;
    return this;
  }
  read(_size) {
    if (this._bodyConsumed) return null;
    this._bodyConsumed = true;
    let buf;
    if (typeof Buffer !== "undefined") {
      buf = this._isBinary ? Buffer.from(this._body, "binary") : Buffer.from(this._body);
    } else {
      buf = this._body;
    }
    if (this.listenerCount("data") > 0) this.emit("data", buf);
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
  pipe(dest) {
    let buf;
    if (typeof Buffer !== "undefined") {
      buf = this._isBinary ? Buffer.from(this._body || "", "binary") : Buffer.from(this._body || "");
    } else {
      buf = this._body || "";
    }
    if (typeof dest.write === "function" && (typeof buf === "string" ? buf.length : buf.length) > 0) {
      dest.write(buf);
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
  pause() {
    this._flowing = false;
    this.readableFlowing = false;
    return this;
  }
  resume() {
    this._flowing = true;
    this.readableFlowing = true;
    if (!this._bodyConsumed) {
      Promise.resolve().then(() => {
        if (!this._bodyConsumed) {
          this._bodyConsumed = true;
          if (this._body) {
            let buf;
            if (typeof Buffer !== "undefined") {
              buf = this._isBinary ? Buffer.from(this._body, "binary") : Buffer.from(this._body);
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
  unpipe(_dest) {
    return this;
  }
  destroy(err) {
    this.destroyed = true;
    this.readable = false;
    if (err) this.emit("error", err);
    this._emitClose();
    return this;
  }
  _abort(err = createConnResetError("aborted")) {
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
  _emitClose() {
    if (this._closeEmitted) {
      return;
    }
    this._closeEmitted = true;
    this.emit("close");
  }
  [Symbol.asyncIterator]() {
    const self = this;
    let dataEmitted = false;
    let ended = false;
    return {
      async next() {
        if (ended || self._ended) {
          return { done: true, value: void 0 };
        }
        if (!dataEmitted && !self._bodyConsumed) {
          dataEmitted = true;
          self._bodyConsumed = true;
          let buf;
          if (typeof Buffer !== "undefined") {
            buf = self._isBinary ? Buffer.from(self._body || "", "binary") : Buffer.from(self._body || "");
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
        return { done: true, value: void 0 };
      },
      return() {
        ended = true;
        return Promise.resolve({ done: true, value: void 0 });
      },
      throw(err) {
        ended = true;
        self.emit("error", err);
        return Promise.resolve({ done: true, value: void 0 });
      }
    };
  }
};

var ClientRequest = class {
  _options;
  _callback;
  _listeners = {};
  _headers = {};
  _rawHeaderNames = /* @__PURE__ */ new Map();
  _body = "";
  _bodyBytes = 0;
  _ended = false;
  _agent;
  _hostKey;
  _socketEndListener = null;
  _socketCloseListener = null;
  _loopbackAbort;
  _response = null;
  _closeEmitted = false;
  _abortEmitted = false;
  _signalAbortHandler;
  _skipExecute = false;
  _destroyError;
  _errorEmitted = false;
  socket;
  finished = false;
  writable = true;
  writableEnded = false;
  writableFinished = false;
  headersSent = false;
  aborted = false;
  destroyed = false;
  path;
  method;
  reusedSocket = false;
  timeoutCb;
  constructor(options, callback) {
    const normalizedMethod = validateRequestMethod(options.method);
    this._options = {
      ...options,
      method: normalizedMethod,
      path: validateRequestPath(options.path)
    };
    this._callback = callback;
    this._validateTimeoutOption();
    this._setOutgoingHeaders(options.headers);
    if (!this._headers.host) {
      this._setHeaderValue("Host", buildHostHeader(this._options));
    }
    this.path = String(this._options.path || "/");
    this.method = String(this._options.method || "GET").toUpperCase();
    const agentOpt = this._options.agent;
    if (agentOpt === false) {
      this._agent = null;
    } else if (agentOpt instanceof Agent) {
      this._agent = agentOpt;
    } else if (this._options._agentOSDefaultAgent instanceof Agent) {
      this._agent = this._options._agentOSDefaultAgent;
    } else {
      this._agent = null;
    }
    this._hostKey = this._agent ? this._agent._getHostKey(this._options) : "";
    this._bindAbortSignal();
    if (typeof this._options.timeout === "number") {
      this.setTimeout(this._options.timeout);
    }
    Promise.resolve().then(() => this._execute());
  }
  _assignSocket(socket, reusedSocket) {
    this.socket = socket;
    this.reusedSocket = reusedSocket;
    const trackedSocket = socket;
    if (!trackedSocket._agentPermanentListenersInstalled) {
      trackedSocket._agentPermanentListenersInstalled = true;
      socket.on("error", () => {
      });
      socket.on("end", () => {
      });
    }
    this._socketEndListener = () => {
    };
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
  _handleSocketError(err) {
    this._emit("error", err);
  }
  _finalizeSocket(socket, keepSocketAlive) {
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
      this._agent._releaseSocket(this._hostKey, socket, this._options, keepSocketAlive);
    } else if (!socket.destroyed) {
      socket.destroy();
    }
  }
  async _dispatchWithSocket(socket) {
    this.headersSent = true;
    try {
      const normalizedHeaders = normalizeRequestHeaders(this._options.headers);
      const requestMethod = String(this._options.method || "GET").toUpperCase();
      const bridgeBackedSocket = socket instanceof NetSocket || (typeof socket?._socketId === "string" && socket._socketId.length > 0) || (typeof socket?._socketId === "number" && socket._socketId > 0);
      // Bridge-backed sockets already speak kernel-routed byte streams, so route
      // HTTP requests through the raw serializer instead of undici's dispatcher.
      if (bridgeBackedSocket || socket?._loopbackServer || isRawSocketRequest(requestMethod, normalizedHeaders) || this._options.socketPath || this._agent?.keepAlive === true) {
        await this._dispatchRawSocketRequest(socket, requestMethod, normalizedHeaders);
      } else {
        await this._dispatchUndiciRequest(socket, requestMethod);
      }
    } catch (err) {
      this._clearTimeout();
      this._emit("error", err);
      this._finalizeSocket(socket, false);
    }
  }
  async _dispatchUndiciRequest(socket, requestMethod) {
    await waitForSocketReadyForProtocol(socket, this._options.protocol || "http:");
    const dispatcher = getUndiciClientForSocket(socket, this._options);
    const bodyBuffer = this._body ? Buffer.from(this._body) : Buffer.alloc(0);
    const headerPairs = buildRawHttpHeaderPairs(this._headers, this._rawHeaderNames);
    if (bodyBuffer.length > 0 && !this._headers["content-length"] && !this._headers["transfer-encoding"]) {
      headerPairs.push(["Content-Length", String(bodyBuffer.length)]);
    }
    const response = await new Promise((resolve, reject) => {
      try {
        undiciRequest.call(dispatcher, {
          path: this._options.path || "/",
          method: requestMethod,
          headers: flattenHeaderPairs(headerPairs),
          body: bodyBuffer.length > 0 ? bodyBuffer : null,
          signal: this._options.signal,
          responseHeaders: "raw"
        }, (err, result) => {
          if (err) {
            reject(err);
            return;
          }
          resolve(result);
        });
      } catch (error) {
        reject(error);
      }
    });
    const responseBody = await readUndiciReadableBody(response?.body);
    await new Promise((resolve) => {
      queueMicrotask(resolve);
    });
    this.finished = true;
    this._clearTimeout();
    const res = new IncomingMessage({
      status: response?.statusCode,
      statusText: response?.statusText,
      headers: Array.isArray(response?.headers) ? response.headers : [],
      rawHeaders: Array.isArray(response?.headers) ? response.headers : [],
      trailers: response?.trailers && typeof response.trailers === "object" ? response.trailers : {},
      body: responseBody.length > 0 ? responseBody.toString("base64") : "",
      bodyEncoding: "base64",
      url: this._buildUrl()
    });
    this._response = res;
    res.socket = socket;
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
  }
  async _dispatchRawSocketRequest(socket, requestMethod, normalizedHeaders) {
    const protocol = this._options.protocol || "http:";
    await waitForSocketReadyForProtocol(socket, protocol);
    const bodyBuffer = this._body ? Buffer.from(this._body) : Buffer.alloc(0);
    const headerPairs = buildRawHttpHeaderPairs(this._headers, this._rawHeaderNames);
    if (bodyBuffer.length > 0 && !normalizedHeaders["content-length"] && !normalizedHeaders["transfer-encoding"]) {
      headerPairs.push(["Content-Length", String(bodyBuffer.length)]);
    }
    const requestBuffer = serializeRawHttpRequest(
      requestMethod,
      this._options.path || "/",
      headerPairs,
      bodyBuffer
    );
    const timeoutMs = typeof this._options.timeout === "number" && this._options.timeout > 0 ? this._options.timeout : 3e4;
    const responsePromise = waitForRawHttpResponse(socket, requestMethod, timeoutMs);
    socket.write(requestBuffer);
    const response = await responsePromise;
    this.finished = true;
    this._clearTimeout();
    if (response.status === 101) {
      const res2 = new IncomingMessage({
        status: response.status,
        statusText: response.statusText,
        headers: response.headers,
        rawHeaders: response.rawHeaders,
        body: "",
        bodyEncoding: "base64",
        url: this._buildUrl()
      });
      this._response = res2;
      res2.socket = socket;
      const head = response.head ?? Buffer.alloc(0);
      if (this._listenerCount("upgrade") === 0) {
        socket.destroy();
        return;
      }
      this._emit("upgrade", res2, socket, head);
      return;
    }
    if (requestMethod === "CONNECT") {
      const res2 = new IncomingMessage({
        status: response.status,
        statusText: response.statusText,
        headers: response.headers,
        rawHeaders: response.rawHeaders,
        body: "",
        bodyEncoding: "base64",
        url: this._buildUrl()
      });
      this._response = res2;
      res2.socket = socket;
      const head = response.head ?? Buffer.alloc(0);
      this._emit("connect", res2, socket, head);
      return;
    }
    const res = new IncomingMessage({
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
      rawHeaders: response.rawHeaders,
      body: response.body && response.body.length > 0 ? response.body.toString("base64") : "",
      bodyEncoding: "base64",
      url: this._buildUrl()
    });
    this._response = res;
    res.socket = socket;
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
  }
  _execute() {
    if (this._skipExecute) {
      return;
    }
    if (this._agent) {
      this._agent.addRequest(this, this._options);
      return;
    }
    const finish = (socket) => {
      if (!socket) {
        this._handleSocketError(new Error("Failed to create socket"));
        this._emitClose();
        return;
      }
      this._assignSocket(socket, false);
    };
    const createConnection = this._options.createConnection;
    if (typeof createConnection === "function") {
      // Node keeps the HTTP request target separate from the options object
      // passed to transport creation. Connection factories such as `ws` mutate
      // `options.path` to `options.socketPath`; sharing our request state would
      // silently rewrite a WebSocket request target to `/` before serialization.
      const maybeSocket = createConnection({ ...this._options }, (_err, socket) => {
        finish(socket);
      });
      finish(maybeSocket);
      return;
    }
    finish(createHttpRequestSocket(this._options));
  }
  _buildUrl() {
    const opts = this._options;
    const protocol = opts.protocol || (opts.port === 443 ? "https:" : "http:");
    const host = opts.hostname || opts.host || "localhost";
    const port = opts.port ? ":" + opts.port : "";
    const path = opts.path || "/";
    return protocol + "//" + host + port + path;
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  addListener(event, listener) {
    return this.on(event, listener);
  }
  prependListener(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].unshift(listener);
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    wrapper.listener = listener;
    return this.on(event, wrapper);
  }
  prependOnceListener(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    wrapper.listener = listener;
    return this.prependListener(event, wrapper);
  }
  off(event, listener) {
    if (this._listeners[event]) {
      const idx = this._listeners[event].findIndex(
        (registered) => registered === listener || registered.listener === listener
      );
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  listeners(event) {
    return (this._listeners[event] || []).map(
      (listener) => listener.listener || listener
    );
  }
  listenerCount(event) {
    return this._listenerCount(event);
  }
  emit(event, ...args) {
    const hadListeners = this._listenerCount(event) > 0;
    this._emit(event, ...args);
    return hadListeners;
  }
  getHeader(name) {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE"
      );
    }
    return this._headers[name.toLowerCase()];
  }
  setHeader(name, value) {
    if (this.headersSent) {
      throw createErrorWithCode(
        "Cannot set headers after they are sent to the client",
        "ERR_HTTP_HEADERS_SENT"
      );
    }
    this._setHeaderValue(name, value);
    return this;
  }
  getHeaders() {
    const headers = /* @__PURE__ */ Object.create(null);
    for (const [key, value] of Object.entries(this._headers)) {
      headers[key] = Array.isArray(value) ? [...value] : value;
    }
    return headers;
  }
  getHeaderNames() {
    return Object.keys(this._headers);
  }
  getRawHeaderNames() {
    return Object.keys(this._headers).map((key) => this._rawHeaderNames.get(key) || key);
  }
  hasHeader(name) {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE"
      );
    }
    return Object.prototype.hasOwnProperty.call(this._headers, name.toLowerCase());
  }
  removeHeader(name) {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE"
      );
    }
    const lowerName = name.toLowerCase();
    delete this._headers[lowerName];
    this._rawHeaderNames.delete(lowerName);
    this._options.headers = { ...this._headers };
  }
  _emit(event, ...args) {
    dispatchCustomEmitterListeners(this, this._listeners[event], args);
  }
  _listenerCount(event) {
    return this._listeners[event]?.length || 0;
  }
  _setOutgoingHeaders(headers) {
    this._headers = {};
    this._rawHeaderNames = /* @__PURE__ */ new Map();
    if (!headers) {
      this._options.headers = {};
      return;
    }
    if (Array.isArray(headers)) {
      for (let index = 0; index < headers.length; index += 2) {
        const key = headers[index];
        const value = headers[index + 1];
        if (key !== void 0 && value !== void 0) {
          this._setHeaderValue(String(key), value);
        }
      }
      return;
    }
    Object.entries(headers).forEach(([key, value]) => {
      if (value !== void 0) {
        this._setHeaderValue(key, value);
      }
    });
  }
  _setHeaderValue(name, value) {
    const actualName = validateHeaderName(name).toLowerCase();
    validateHeaderValue(actualName, value);
    this._headers[actualName] = Array.isArray(value) ? value.map((entry) => String(entry)) : String(value);
    if (!this._rawHeaderNames.has(actualName)) {
      this._rawHeaderNames.set(actualName, name);
    }
    this._options.headers = { ...this._headers };
  }
  write(data, _encoding, callback) {
    if (typeof _encoding === "function") callback = _encoding;
    const addedBytes = typeof Buffer !== "undefined" ? Buffer.byteLength(data) : data.length;
    if (this._bodyBytes + addedBytes > MAX_HTTP_BODY_BYTES) {
      throw new Error("ERR_HTTP_BODY_TOO_LARGE: request body exceeds " + MAX_HTTP_BODY_BYTES + " byte limit");
    }
    this._body += data;
    this._bodyBytes += addedBytes;
    if (typeof callback === "function") queueMicrotask(callback);
    return true;
  }
  end(data, encoding, callback) {
    if (typeof data === "function") {
      callback = data;
      data = void 0;
    } else if (typeof encoding === "function") {
      callback = encoding;
      encoding = void 0;
    }
    if (data !== void 0 && data !== null) this.write(data, encoding);
    if (typeof callback === "function") this.once("finish", callback);
    this._ended = true;
    this.writable = false;
    this.writableEnded = true;
    this.writableFinished = true;
    queueMicrotask(() => this._emit("finish"));
    return this;
  }
  abort() {
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
  destroy(err) {
    if (this.destroyed) {
      return this;
    }
    this.destroyed = true;
    this._clearTimeout();
    this._unbindAbortSignal();
    this._loopbackAbort?.();
    this._loopbackAbort = void 0;
    if (!this.socket && err && err.code === "ABORT_ERR") {
      this._skipExecute = true;
    }
    const responseStarted = this._response != null;
    const destroyError = err ?? (!this.aborted && !responseStarted ? createConnResetError() : void 0);
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
  setTimeout(timeout, callback) {
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
  setNoDelay() {
    return this;
  }
  setSocketKeepAlive() {
    return this;
  }
  flushHeaders() {
  }
  _emitClose() {
    if (this._closeEmitted) {
      return;
    }
    this._closeEmitted = true;
    this._emit("close");
  }
  _applyTimeoutToSocket(socket) {
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
  _validateTimeoutOption() {
    const timeout = this._options.timeout;
    if (timeout === void 0) {
      return;
    }
    if (typeof timeout !== "number") {
      const received = timeout === null ? "null" : typeof timeout === "string" ? `type string ('${timeout}')` : `type ${typeof timeout} (${JSON.stringify(timeout)})`;
      const error = new TypeError(`The "timeout" argument must be of type number. Received ${received}`);
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
  }
  _bindAbortSignal() {
    const signal = this._options.signal;
    if (!signal) {
      return;
    }
    this._signalAbortHandler = () => {
      this.destroy(createAbortError2());
    };
    if (signal.aborted) {
      this.destroyed = true;
      this._skipExecute = true;
      queueMicrotask(() => {
        this._emit("error", createAbortError2());
        this._emitClose();
      });
      return;
    }
    if (typeof signal.addEventListener === "function") {
      signal.addEventListener("abort", this._signalAbortHandler, { once: true });
      return;
    }
    const signalWithOnAbort = signal;
    signalWithOnAbort.__agentOsPrevOnAbort__ = signalWithOnAbort.onabort ?? null;
    signalWithOnAbort.onabort = ((event) => {
      signalWithOnAbort.__agentOsPrevOnAbort__?.call(signal, event);
      this._signalAbortHandler?.();
    });
  }
  _unbindAbortSignal() {
    const signal = this._options.signal;
    if (!signal || !this._signalAbortHandler) {
      return;
    }
    if (typeof signal.removeEventListener === "function") {
      signal.removeEventListener("abort", this._signalAbortHandler);
      this._signalAbortHandler = void 0;
      return;
    }
    const signalWithOnAbort = signal;
    if (signalWithOnAbort.onabort === this._signalAbortHandler) {
      signalWithOnAbort.onabort = signalWithOnAbort.__agentOsPrevOnAbort__ ?? null;
    } else if (signalWithOnAbort.__agentOsPrevOnAbort__ !== void 0) {
      signalWithOnAbort.onabort = signalWithOnAbort.__agentOsPrevOnAbort__ ?? null;
    }
    delete signalWithOnAbort.__agentOsPrevOnAbort__;
    this._signalAbortHandler = void 0;
  }
  _clearTimeout() {
    if (this.socket && this.timeoutCb) {
      this.socket.off?.("timeout", this.timeoutCb);
      this.socket.removeListener?.("timeout", this.timeoutCb);
    }
    if (this.socket?.setTimeout) {
      this.socket.setTimeout(0);
    }
  }
};

function createUnsupportedHttpSocketWriteError(surface) {
  return createErrorWithCode(
    `${surface}.write() is not implemented by the agentos http compatibility layer`,
    "ERR_NOT_IMPLEMENTED"
  );
}

var FakeSocket = class {
  remoteAddress;
  remotePort;
  localAddress = "127.0.0.1";
  localPort = 0;
  connecting = false;
  destroyed = false;
  writable = true;
  readable = true;
  timeout = 0;
  _listeners = {};
  _closed = false;
  _closeScheduled = false;
  _timeoutTimer = null;
  _freeTimer = null;
  constructor(options) {
    this.remoteAddress = options?.host || "127.0.0.1";
    this.remotePort = options?.port || 80;
  }
  setTimeout(ms, cb) {
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
  setNoDelay(_noDelay) {
    return this;
  }
  setKeepAlive(_enable, _delay) {
    return this;
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener.call(this, ...args);
    };
    return this.on(event, wrapper);
  }
  off(event, listener) {
    if (this._listeners[event]) {
      const idx = this._listeners[event].indexOf(listener);
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  removeAllListeners(event) {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }
  emit(event, ...args) {
    const handlers = this._listeners[event];
    return dispatchCustomEmitterListeners(this, handlers, args);
  }
  listenerCount(event) {
    return this._listeners[event]?.length || 0;
  }
  listeners(event) {
    return [...this._listeners[event] || []];
  }
  write(_data, _encodingOrCallback, _callback) {
    throw createUnsupportedHttpSocketWriteError("http.ClientRequest.socket");
  }
  end() {
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
  destroy() {
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
};

var DirectTunnelSocket = class {
  remoteAddress;
  remotePort;
  localAddress = "127.0.0.1";
  localPort = 0;
  connecting = false;
  destroyed = false;
  writable = true;
  readable = true;
  readyState = "open";
  bytesWritten = 0;
  _listeners = {};
  _encoding;
  _peer = null;
  _readableState = { endEmitted: false, ended: false };
  _writableState = { finished: false, errorEmitted: false };
  constructor(options) {
    this.remoteAddress = options?.host || "127.0.0.1";
    this.remotePort = options?.port || 80;
  }
  _attachPeer(peer) {
    this._peer = peer;
  }
  setTimeout(_ms, _cb) {
    return this;
  }
  setNoDelay(_noDelay) {
    return this;
  }
  setKeepAlive(_enable, _delay) {
    return this;
  }
  setEncoding(encoding) {
    this._encoding = encoding;
    return this;
  }
  ref() {
    return this;
  }
  unref() {
    return this;
  }
  cork() {
  }
  uncork() {
  }
  pause() {
    return this;
  }
  resume() {
    return this;
  }
  address() {
    return { address: this.localAddress, family: "IPv4", port: this.localPort };
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener.call(this, ...args);
    };
    return this.on(event, wrapper);
  }
  off(event, listener) {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  removeAllListeners(event) {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }
  emit(event, ...args) {
    const listeners = this._listeners[event];
    return dispatchCustomEmitterListeners(this, listeners, args);
  }
  listenerCount(event) {
    return this._listeners[event]?.length || 0;
  }
  write(data, encodingOrCb, cb) {
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
  end(data) {
    if (data !== void 0) {
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
  destroy(err) {
    if (this.destroyed) return this;
    this.destroyed = true;
    this.readable = false;
    this.writable = false;
    this._readableState.endEmitted = true;
    this._readableState.ended = true;
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
  _pushData(buffer) {
    if (!this.readable || this.destroyed) {
      return;
    }
    this.emit("data", this._encoding ? buffer.toString(this._encoding) : buffer);
  }
  _pushEnd() {
    if (this.destroyed) {
      return;
    }
    this.readable = false;
    this.writable = false;
    this._readableState.endEmitted = true;
    this._readableState.ended = true;
    this._writableState.finished = true;
    this.emit("end");
    this.emit("close", false);
  }
};

function normalizeSocketChunk(data) {
  if (typeof Buffer !== "undefined" && Buffer.isBuffer(data)) {
    return data;
  }
  if (data instanceof Uint8Array) {
    return Buffer.from(data);
  }
  return Buffer.from(String(data));
}

var Agent = class _Agent {
  static defaultMaxSockets = Infinity;
  options;
  maxSockets;
  maxTotalSockets;
  maxFreeSockets;
  keepAlive;
  keepAliveMsecs;
  timeout;
  requests;
  sockets;
  freeSockets;
  totalSocketCount;
  _listeners = {};
  constructor(options) {
    this.options = { ...options };
    this._validateSocketCountOption("maxSockets", options?.maxSockets);
    this._validateSocketCountOption("maxFreeSockets", options?.maxFreeSockets);
    this._validateSocketCountOption("maxTotalSockets", options?.maxTotalSockets);
    this.keepAlive = options?.keepAlive ?? false;
    this.keepAliveMsecs = options?.keepAliveMsecs ?? 1e3;
    this.maxSockets = options?.maxSockets ?? _Agent.defaultMaxSockets;
    this.maxTotalSockets = options?.maxTotalSockets ?? Infinity;
    this.maxFreeSockets = options?.maxFreeSockets ?? 256;
    this.timeout = options?.timeout ?? -1;
    this.requests = {};
    this.sockets = {};
    this.freeSockets = {};
    this.totalSocketCount = 0;
  }
  _validateSocketCountOption(name, value) {
    if (value === void 0) return;
    if (typeof value !== "number") {
      const received = typeof value === "string" ? `type string ('${value}')` : `type ${typeof value} (${JSON.stringify(value)})`;
      const err = new TypeError(
        `The "${name}" argument must be of type number. Received ${received}`
      );
      err.code = "ERR_INVALID_ARG_TYPE";
      throw err;
    }
    if (Number.isNaN(value) || value <= 0) {
      const err = new RangeError(
        `The value of "${name}" is out of range. It must be > 0. Received ${String(value)}`
      );
      err.code = "ERR_OUT_OF_RANGE";
      throw err;
    }
  }
  getName(options) {
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
  _getHostKey(options) {
    return this.getName(options);
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }
  off(event, listener) {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  emit(event, ...args) {
    const listeners = this._listeners[event];
    return dispatchCustomEmitterListeners(this, listeners, args);
  }
  createConnection(options, cb) {
    const createConnection = typeof options.createConnection === "function" ? options.createConnection : typeof this.options.createConnection === "function" ? this.options.createConnection : null;
    if (createConnection) {
      return createConnection(
        options,
        cb ?? (() => void 0)
      );
    }
    return createHttpRequestSocket(options, cb);
  }
  createSocket(_request, options, cb) {
    let callbackCalled = false;
    const finish = (error, socket) => {
      if (callbackCalled) return;
      callbackCalled = true;
      cb?.(error, socket);
    };
    const socket = this.createConnection(options, finish);
    if (socket) finish(null, socket);
    return socket;
  }
  addRequest(request, options) {
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
  _releaseSocket(name, socket, options, keepSocketAlive) {
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
  _removeSocketCompletely(name, socket) {
    if (socket._freeTimer) {
      clearTimeout(socket._freeTimer);
      socket._freeTimer = null;
    }
    const removed = this._removeSocket(this.sockets, name, socket) || this._removeSocket(this.freeSockets, name, socket);
    if (removed) {
      this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
      Promise.resolve().then(() => this._processPendingRequests());
    }
  }
  _canCreateSocket(name) {
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
  _takeFreeSocket(name) {
    const freeList = this.freeSockets[name];
    while (freeList && freeList.length > 0) {
      const socket = freeList.shift();
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
  _activateSocket(name, socket) {
    const activeList = this.sockets[name] ?? (this.sockets[name] = []);
    activeList.push(socket);
  }
  _createSocketForRequest(name, request, options) {
    let settled = false;
    const finish = (err, socket) => {
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
      keepAliveInitialDelay: this.keepAliveMsecs
    };
    try {
      const maybeSocket = this.createSocket(request, connectionOptions, (err, socket) => {
        finish(err, socket);
      });
      if (maybeSocket) {
        finish(null, maybeSocket);
      }
    } catch (err) {
      finish(err instanceof Error ? err : new Error(String(err)));
    }
  }
  _processPendingRequests() {
    for (const name of Object.keys(this.requests)) {
      const queue = this.requests[name];
      while (queue && queue.length > 0) {
        const freeSocket = this._takeFreeSocket(name);
        if (freeSocket) {
          const entry2 = queue.shift();
          if (entry2.request.destroyed) {
            this._activateSocket(name, freeSocket);
            this._releaseSocket(name, freeSocket, entry2.options, true);
            continue;
          }
          this._activateSocket(name, freeSocket);
          entry2.request._assignSocket(freeSocket, true);
          continue;
        }
        if (!this._canCreateSocket(name)) {
          break;
        }
        const entry = queue.shift();
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
  _removeSocket(sockets, name, socket) {
    const list = sockets[name];
    if (!list) return false;
    const index = list.indexOf(socket);
    if (index === -1) return false;
    list.splice(index, 1);
    if (list.length === 0) delete sockets[name];
    return true;
  }
  _evictFreeSocket(preferredName) {
    const keys = Object.keys(this.freeSockets);
    const orderedKeys = keys.includes(preferredName) ? [...keys.filter((key) => key !== preferredName), preferredName] : keys;
    for (const key of orderedKeys) {
      const socket = this.freeSockets[key]?.[0];
      if (!socket) continue;
      socket.destroy();
      return;
    }
  }
  destroy() {
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
};

function debugBridgeNetwork(...args) {
  if (process.env.AGENTOS_DEBUG_HTTP_BRIDGE === "1") {
    console.error("[agentos bridge network]", ...args);
  }
}

var nextServerId = 1;

var serverInstances = /* @__PURE__ */ new Map();

var HTTP_METHODS = [
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
  "UNSUBSCRIBE"
];

var INVALID_REQUEST_PATH_REGEXP = /[^\u0021-\u00ff]/;

var HTTP_TOKEN_EXTRA_CHARS = /* @__PURE__ */ new Set(["!", "#", "$", "%", "&", "'", "*", "+", "-", ".", "^", "_", "`", "|", "~"]);

function createTypeErrorWithCode(message, code) {
  const error = new TypeError(message);
  error.code = code;
  return error;
}

function createErrorWithCode(message, code) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function formatReceivedType(value) {
  if (value === null) {
    return "null";
  }
  if (Array.isArray(value)) {
    return "an instance of Array";
  }
  const valueType = typeof value;
  if (valueType === "function") {
    const name = typeof value.name === "string" && value.name.length > 0 ? value.name : "anonymous";
    return `function ${name}`;
  }
  if (valueType === "object") {
    const ctorName = value && typeof value === "object" && typeof value.constructor?.name === "string" ? value.constructor.name : "Object";
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

function createInvalidArgTypeError2(argumentName, expectedType, value) {
  return createTypeErrorWithCode(
    `The "${argumentName}" property must be of type ${expectedType}. Received ${formatReceivedType(value)}`,
    "ERR_INVALID_ARG_TYPE"
  );
}

function checkIsHttpToken(value) {
  if (value.length === 0) {
    return false;
  }
  for (let index = 0; index < value.length; index += 1) {
    const char = value[index];
    const code = value.charCodeAt(index);
    const isAlphaNum = code >= 48 && code <= 57 || code >= 65 && code <= 90 || code >= 97 && code <= 122;
    if (!isAlphaNum && !HTTP_TOKEN_EXTRA_CHARS.has(char)) {
      return false;
    }
  }
  return true;
}

function checkInvalidHeaderChar(value) {
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

function validateHeaderName(name, label = "Header name") {
  const actualName = String(name);
  if (!checkIsHttpToken(actualName)) {
    throw createTypeErrorWithCode(
      `${label} must be a valid HTTP token [${JSON.stringify(actualName)}]`,
      "ERR_INVALID_HTTP_TOKEN"
    );
  }
  return actualName;
}

function validateHeaderValue(name, value) {
  if (value === void 0) {
    throw createTypeErrorWithCode(
      `Invalid value "undefined" for header "${name}"`,
      "ERR_HTTP_INVALID_HEADER_VALUE"
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
      "ERR_INVALID_CHAR"
    );
  }
}

function serializeHeaderValue(value) {
  if (Array.isArray(value)) {
    return value.map((entry) => String(entry));
  }
  return String(value);
}

function joinHeaderValue(value) {
  return Array.isArray(value) ? value.join(", ") : value;
}

function cloneStoredHeaderValue(value) {
  return Array.isArray(value) ? [...value] : value;
}

function appendNormalizedHeader(target, key, value) {
  if (key === "set-cookie") {
    const existing2 = target[key];
    if (existing2 === void 0) {
      target[key] = [value];
    } else if (Array.isArray(existing2)) {
      existing2.push(value);
    } else {
      target[key] = [existing2, value];
    }
    return;
  }
  const existing = target[key];
  target[key] = existing === void 0 ? value : `${joinHeaderValue(existing)}, ${value}`;
}

function validateRequestMethod(method) {
  if (method == null || method === "") {
    return void 0;
  }
  if (typeof method !== "string") {
    throw createInvalidArgTypeError2("options.method", "string", method);
  }
  return validateHeaderName(method, "Method");
}

function validateRequestPath(path) {
  const resolvedPath = path == null || path === "" ? "/" : String(path);
  if (INVALID_REQUEST_PATH_REGEXP.test(resolvedPath)) {
    throw createTypeErrorWithCode(
      "Request path contains unescaped characters",
      "ERR_UNESCAPED_CHARACTERS"
    );
  }
  return resolvedPath;
}

function buildHostHeader(options) {
  const host = String(options.hostname || options.host || "localhost");
  const defaultPort = options.protocol === "https:" || Number(options.port) === 443 ? 443 : 80;
  const port = options.port != null ? Number(options.port) : defaultPort;
  return port === defaultPort ? host : `${host}:${port}`;
}

function isFlatHeaderList(headers) {
  return Array.isArray(headers) && (headers.length === 0 || typeof headers[0] === "string");
}

function normalizeRequestHeaders(headers) {
  if (!headers) return {};
  if (Array.isArray(headers)) {
    const normalized2 = {};
    for (let i = 0; i < headers.length; i += 2) {
      const key = headers[i];
      const value = headers[i + 1];
      if (key !== void 0 && value !== void 0) {
        const normalizedKey = validateHeaderName(key).toLowerCase();
        validateHeaderValue(normalizedKey, value);
        appendNormalizedHeader(normalized2, normalizedKey, String(value));
      }
    }
    return normalized2;
  }
  const normalized = {};
  Object.entries(headers).forEach(([key, value]) => {
    if (value === void 0) return;
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

function hasUpgradeRequestHeaders(headers) {
  const connectionHeader = joinHeaderValue(headers.connection || "").toLowerCase();
  return connectionHeader.includes("upgrade") && Boolean(headers.upgrade);
}

function isRawSocketRequest(method, headers) {
  if (String(method || "GET").toUpperCase() === "CONNECT") {
    return true;
  }
  return hasUpgradeRequestHeaders(headers);
}

function socketReadyEventNameForProtocol(protocol) {
  return protocol === "https:" ? "secureConnect" : "connect";
}

function isSocketReadyForProtocol(socket, protocol) {
  if (!socket || socket.destroyed === true) {
    return false;
  }
  if (protocol === "https:") {
    return socket.encrypted === true && socket._tlsUpgrading !== true;
  }
  if (socket._connected === true || socket._loopbackServer) {
    return true;
  }
  if (typeof socket._socketId === "number") {
    return false;
  }
  return socket.connecting === false;
}

function waitForSocketReadyForProtocol(socket, protocol) {
  if (isSocketReadyForProtocol(socket, protocol)) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const readyEvent = socketReadyEventNameForProtocol(protocol);
    const onReady = () => {
      cleanup();
      resolve();
    };
    const onError = (error) => {
      cleanup();
      reject(error instanceof Error ? error : new Error(String(error)));
    };
    const onClose = () => {
      cleanup();
      reject(createConnResetError("socket closed before request was ready"));
    };
    const cleanup = () => {
      socket.off?.(readyEvent, onReady);
      socket.removeListener?.(readyEvent, onReady);
      socket.off?.("error", onError);
      socket.removeListener?.("error", onError);
      socket.off?.("close", onClose);
      socket.removeListener?.("close", onClose);
    };
    socket.once(readyEvent, onReady);
    socket.once("error", onError);
    socket.once("close", onClose);
  });
}

function buildUndiciOrigin(options) {
  const protocol = options?.protocol === "https:" ? "https:" : "http:";
  const hostname = String(options?.hostname || options?.host || "localhost");
  const defaultPort = protocol === "https:" ? 443 : 80;
  const port = Number(options?.port) || defaultPort;
  const originUrl = new URL(`${protocol}//${hostname}`);
  if (port !== defaultPort) {
    originUrl.port = String(port);
  }
  return originUrl.origin;
}

function getUndiciClientForSocket(socket, options) {
  if (typeof UndiciClient !== "function" || typeof undiciRequest !== "function") {
    throw new Error("Undici request transport is not available");
  }
  const origin = buildUndiciOrigin(options);
  if (socket._agentOSUndiciClient && socket._agentOSUndiciOrigin === origin && socket._agentOSUndiciClient.destroyed !== true) {
    return socket._agentOSUndiciClient;
  }
  const client = new UndiciClient(origin, {
    pipelining: 1,
    connect(_connectOptions, callback) {
      callback(null, socket);
      return socket;
    }
  });
  const clearClient = () => {
    if (socket._agentOSUndiciClient === client) {
      socket._agentOSUndiciClient = null;
      socket._agentOSUndiciOrigin = null;
    }
  };
  socket.once?.("close", clearClient);
  socket._agentOSUndiciClient = client;
  socket._agentOSUndiciOrigin = origin;
  return client;
}

function createHttpRequestSocket(options, callback) {
  const protocol = options?.protocol === "https:" ? "https:" : "http:";
  const host = String(options?.hostname || options?.host || "localhost");
  const port = Number(options?.port) || (protocol === "https:" ? 443 : 80);
	  const socket = protocol === "https:" ? tlsConnect({
	    host,
	    localAddress: options?.localAddress,
	    localPort: options?.localPort,
	    port,
	    servername: options?.servername || host,
	    rejectUnauthorized: options?.rejectUnauthorized,
	    socket: options?.socket
	  }) : netConnect({
	    host,
	    localAddress: options?.localAddress,
	    localPort: options?.localPort,
	    port,
    path: options?.socketPath,
    keepAlive: options?.keepAlive,
    keepAliveInitialDelay: options?.keepAliveInitialDelay
  });
  if (callback) {
    const readyEvent = socketReadyEventNameForProtocol(protocol);
    const onReady = () => {
      cleanup();
      callback(null, socket);
    };
    const onError = (error) => {
      cleanup();
      callback(error instanceof Error ? error : new Error(String(error)));
    };
    const cleanup = () => {
      socket.off?.(readyEvent, onReady);
      socket.removeListener?.(readyEvent, onReady);
      socket.off?.("error", onError);
      socket.removeListener?.("error", onError);
    };
    socket.once(readyEvent, onReady);
    socket.once("error", onError);
  }
  return socket;
}

function flattenHeaderPairs(headerPairs) {
  const flattened = [];
  for (const [name, value] of headerPairs) {
    flattened.push(name, value);
  }
  return flattened;
}

function buildRawHttpHeaderPairs(headers, rawHeaderNames) {
  const pairs = [];
  Object.entries(headers).forEach(([key, value]) => {
    const rawName = rawHeaderNames.get(key) || key;
    if (Array.isArray(value)) {
      value.forEach((entry) => {
        pairs.push([rawName, String(entry)]);
      });
      return;
    }
    pairs.push([rawName, String(value)]);
  });
  return pairs;
}

function serializeRawHttpRequest(method, path, headerPairs, bodyBuffer) {
  const lines = [`${method} ${path} HTTP/1.1`];
  headerPairs.forEach(([name, value]) => {
    lines.push(`${name}: ${value}`);
  });
  lines.push("", "");
  const headerBuffer = Buffer.from(lines.join("\r\n"), "latin1");
  if (!bodyBuffer || bodyBuffer.length === 0) {
    return headerBuffer;
  }
  return Buffer.concat([headerBuffer, bodyBuffer]);
}

async function readUndiciReadableBody(body) {
  if (!body) {
    return Buffer.alloc(0);
  }
  const chunks = [];
  for await (const chunk of body) {
    if (typeof Buffer !== "undefined" && Buffer.isBuffer(chunk)) {
      chunks.push(chunk);
    } else if (chunk instanceof Uint8Array) {
      chunks.push(Buffer.from(chunk));
    } else {
      chunks.push(Buffer.from(String(chunk)));
    }
  }
  if (chunks.length === 0) {
    return Buffer.alloc(0);
  }
  return chunks.length === 1 ? chunks[0] : Buffer.concat(chunks);
}

function parseRawHttpResponse(buffer) {
  const headerEnd = buffer.indexOf("\r\n\r\n");
  if (headerEnd === -1) {
    return null;
  }
  const headText = buffer.subarray(0, headerEnd).toString("latin1");
  const lines = headText.split("\r\n");
  const statusLine = lines.shift() || "";
  const statusMatch = /^HTTP\/(\d)\.(\d)\s+(\d{3})(?:\s+(.*))?$/.exec(statusLine);
  if (!statusMatch) {
    throw new Error(`Invalid HTTP response status line: ${statusLine}`);
  }
  const headers = {};
  const rawHeaders = [];
  let previousHeaderName = null;
  for (const line of lines) {
    if (!line) {
      continue;
    }
    if ((line.startsWith(" ") || line.startsWith("\t")) && rawHeaders.length >= 2 && previousHeaderName) {
      const continuation = line.trim();
      rawHeaders[rawHeaders.length - 1] += ` ${continuation}`;
      headers[previousHeaderName] = joinHeaderValue(headers[previousHeaderName]) + ` ${continuation}`;
      continue;
    }
    const separatorIndex = line.indexOf(":");
    if (separatorIndex === -1) {
      throw new Error(`Invalid HTTP response header line: ${line}`);
    }
    const rawName = line.slice(0, separatorIndex);
    const rawValue = line.slice(separatorIndex + 1).trim();
    previousHeaderName = rawName.toLowerCase();
    rawHeaders.push(rawName, rawValue);
    appendNormalizedHeader(headers, previousHeaderName, rawValue);
  }
  return {
    status: Number(statusMatch[3]),
    statusText: statusMatch[4] || "",
    headers,
    rawHeaders,
    head: buffer.subarray(headerEnd + 4)
  };
}

function waitForRawHttpResponseHead(socket, timeoutMs) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);
    let settled = false;
    const finish = (error, value) => {
      if (settled) {
        return;
      }
      settled = true;
      cleanup();
      if (error) {
        reject(error);
        return;
      }
      resolve(value);
    };
    const cleanup = () => {
      clearTimeout(timer);
      socket.off?.("data", onData);
      socket.removeListener?.("data", onData);
      socket.off?.("error", onError);
      socket.removeListener?.("error", onError);
      socket.off?.("end", onEnd);
      socket.removeListener?.("end", onEnd);
      socket.off?.("close", onClose);
      socket.removeListener?.("close", onClose);
    };
    const onData = (chunk) => {
      const payload = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      buffer = Buffer.concat([buffer, payload]);
      try {
        const parsed = parseRawHttpResponse(buffer);
        if (parsed) {
          finish(null, parsed);
        }
      } catch (error) {
        finish(error instanceof Error ? error : new Error(String(error)));
      }
    };
    const onError = (error) => {
      finish(error instanceof Error ? error : new Error(String(error)));
    };
    const onEnd = () => {
      finish(createConnResetError("socket ended before receiving HTTP response head"));
    };
    const onClose = () => {
      finish(createConnResetError("socket closed before receiving HTTP response head"));
    };
    const timer = setTimeout(() => {
      finish(new Error(`Timed out waiting for HTTP response head after ${timeoutMs}ms`));
    }, timeoutMs);
    socket.on("data", onData);
    socket.once("error", onError);
    socket.once("end", onEnd);
    socket.once("close", onClose);
  });
}

function waitForRawHttpResponse(socket, requestMethod, timeoutMs) {
  return new Promise((resolve, reject) => {
    let header = null;
    let bodyBuffer = Buffer.alloc(0);
    let expectedContentLength = null;
    let expectsChunkedBody = false;
    let expectsCloseDelimitedBody = false;
    let settled = false;
    const finish = (error, value) => {
      if (settled) {
        return;
      }
      settled = true;
      cleanup();
      if (error) {
        reject(error);
        return;
      }
      resolve(value);
    };
    const cleanup = () => {
      clearTimeout(timer);
      socket.off?.("data", onData);
      socket.removeListener?.("data", onData);
      socket.off?.("error", onError);
      socket.removeListener?.("error", onError);
      socket.off?.("end", onEnd);
      socket.removeListener?.("end", onEnd);
      socket.off?.("close", onClose);
      socket.removeListener?.("close", onClose);
    };
    const maybeFinishWithBody = () => {
      if (!header) {
        return false;
      }
      if (!hasResponseBody(header.status, requestMethod)) {
        finish(null, {
          ...header,
          body: Buffer.alloc(0)
        });
        return true;
      }
      if (expectsChunkedBody) {
        const parsedChunked = parseChunkedBody(bodyBuffer);
        if (parsedChunked === null) {
          finish(new Error("Invalid chunked HTTP response body"));
          return true;
        }
        if (!parsedChunked.complete) {
          return false;
        }
        finish(null, {
          ...header,
          body: parsedChunked.body
        });
        return true;
      }
      if (expectedContentLength !== null) {
        if (bodyBuffer.length < expectedContentLength) {
          return false;
        }
        finish(null, {
          ...header,
          body: bodyBuffer.subarray(0, expectedContentLength)
        });
        return true;
      }
      return false;
    };
    const configureBodyHandling = () => {
      if (!header || !hasResponseBody(header.status, requestMethod)) {
        return;
      }
      const transferEncoding = header.headers["transfer-encoding"];
      const contentLength = header.headers["content-length"];
      if (transferEncoding !== void 0) {
        const tokens = splitTransferEncodingTokens(joinHeaderValue(transferEncoding));
        const chunkedCount = tokens.filter((entry) => entry === "chunked").length;
        const hasChunked = chunkedCount > 0;
        const chunkedIsFinal = hasChunked && tokens[tokens.length - 1] === "chunked";
        if (!hasChunked || chunkedCount !== 1 || !chunkedIsFinal || contentLength !== void 0) {
          throw new Error("Unsupported transfer-encoding in HTTP response");
        }
        expectsChunkedBody = true;
        return;
      }
      if (contentLength !== void 0) {
        const parsedContentLength = parseContentLengthHeader(contentLength);
        if (parsedContentLength === null) {
          throw new Error("Invalid content-length in HTTP response");
        }
        expectedContentLength = parsedContentLength;
        return;
      }
      expectsCloseDelimitedBody = true;
    };
    const onData = (chunk) => {
      const payload = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      if (!header) {
        bodyBuffer = Buffer.concat([bodyBuffer, payload]);
        try {
          const parsed = parseRawHttpResponse(bodyBuffer);
          if (!parsed) {
            return;
          }
          header = parsed;
          bodyBuffer = Buffer.from(parsed.head);
          configureBodyHandling();
          maybeFinishWithBody();
        } catch (error) {
          finish(error instanceof Error ? error : new Error(String(error)));
        }
        return;
      }
      bodyBuffer = Buffer.concat([bodyBuffer, payload]);
      try {
        maybeFinishWithBody();
      } catch (error) {
        finish(error instanceof Error ? error : new Error(String(error)));
      }
    };
    const onError = (error) => {
      finish(error instanceof Error ? error : new Error(String(error)));
    };
    const onEnd = () => {
      if (!header) {
        finish(createConnResetError("socket ended before receiving HTTP response head"));
        return;
      }
      if (expectsCloseDelimitedBody) {
        finish(null, {
          ...header,
          body: bodyBuffer
        });
        return;
      }
      if (maybeFinishWithBody()) {
        return;
      }
      finish(createConnResetError("socket ended before receiving complete HTTP response body"));
    };
    const onClose = () => {
      if (!header) {
        finish(createConnResetError("socket closed before receiving HTTP response head"));
        return;
      }
      if (expectsCloseDelimitedBody) {
        finish(null, {
          ...header,
          body: bodyBuffer
        });
        return;
      }
      if (maybeFinishWithBody()) {
        return;
      }
      finish(createConnResetError("socket closed before receiving complete HTTP response body"));
    };
    const timer = setTimeout(() => {
      finish(new Error(`Timed out waiting for HTTP response after ${timeoutMs}ms`));
    }, timeoutMs);
    socket.on("data", onData);
    socket.once("error", onError);
    socket.once("end", onEnd);
    socket.once("close", onClose);
  });
}

function hasResponseBody(statusCode, method) {
  if (method === "HEAD") {
    return false;
  }
  if (statusCode >= 100 && statusCode < 200 || statusCode === 204 || statusCode === 304) {
    return false;
  }
  return true;
}

function splitTransferEncodingTokens(value) {
  return value.split(",").map((entry) => entry.trim().toLowerCase()).filter((entry) => entry.length > 0);
}

function parseContentLengthHeader(value) {
  if (value === void 0) {
    return 0;
  }
  const entries = Array.isArray(value) ? value : [value];
  let parsed = null;
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

function parseChunkedBody(bodyBuffer, maxBodyBytes = MAX_HTTP_BODY_BYTES) {
  let offset = 0;
  let totalBodyBytes = 0;
  const chunks = [];
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
    if (extensionPart !== void 0 && /[\r\n]/.test(extensionPart)) {
      return null;
    }
    const chunkSize = Number.parseInt(sizePart, 16);
    if (!Number.isSafeInteger(chunkSize) || chunkSize < 0) {
      return null;
    }
    if (totalBodyBytes + chunkSize > maxBodyBytes) {
      return null;
    }
    const chunkStart = lineEnd + 2;
    if (chunkSize === 0) {
      const trailersStart = chunkStart;
      if (trailersStart === bodyBuffer.length) {
        return { complete: false };
      }
      if (bodyBuffer[trailersStart] === 13 && bodyBuffer[trailersStart + 1] === 10) {
        return {
          complete: true,
          bytesConsumed: trailersStart + 2,
          body: chunks.length > 0 ? Buffer.concat(chunks) : Buffer.alloc(0)
        };
      }
      const trailersEnd = bodyBuffer.indexOf("\r\n\r\n", trailersStart);
      if (trailersEnd === -1) {
        return { complete: false };
      }
      const trailerBlock = bodyBuffer.subarray(trailersStart, trailersEnd).toString("latin1");
      if (trailerBlock.length > 0) {
        for (const trailerLine of trailerBlock.split("\r\n")) {
          if (trailerLine.length === 0) {
            continue;
          }
          if (trailerLine.startsWith(" ") || trailerLine.startsWith("	")) {
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
        body: chunks.length > 0 ? Buffer.concat(chunks) : Buffer.alloc(0)
      };
    }
    const chunkEnd = chunkStart + chunkSize;
    const chunkTerminatorEnd = chunkEnd + 2;
    if (chunkTerminatorEnd > bodyBuffer.length) {
      return { complete: false };
    }
    if (bodyBuffer[chunkEnd] !== 13 || bodyBuffer[chunkEnd + 1] !== 10) {
      return null;
    }
    totalBodyBytes += chunkSize;
    chunks.push(bodyBuffer.subarray(chunkStart, chunkEnd));
    offset = chunkTerminatorEnd;
  }
}

function parseLoopbackRequestBuffer(buffer, server) {
  let requestStart = 0;
  while (requestStart + 1 < buffer.length && buffer[requestStart] === 13 && buffer[requestStart + 1] === 10) {
    requestStart += 2;
  }
  const headerEnd = buffer.indexOf("\r\n\r\n", requestStart);
  if (headerEnd === -1) {
    if (buffer.length - requestStart > MAX_HTTP_REQUEST_HEADER_BYTES) {
      return {
        kind: "bad-request",
        closeConnection: true
      };
    }
    return { kind: "incomplete" };
  }
  if (headerEnd - requestStart > MAX_HTTP_REQUEST_HEADER_BYTES) {
    return {
      kind: "bad-request",
      closeConnection: true
    };
  }
  const headerBlock = buffer.subarray(requestStart, headerEnd).toString("latin1");
  const [requestLine, ...headerLines] = headerBlock.split("\r\n");
  if (headerLines.length > MAX_HTTP_REQUEST_HEADERS) {
    return {
      kind: "bad-request",
      closeConnection: true
    };
  }
  const requestMatch = /^([A-Z]+)\s+(\S+)\s+HTTP\/(1)\.(0|1)$/.exec(requestLine);
  if (!requestMatch) {
    return {
      kind: "bad-request",
      closeConnection: true
    };
  }
  const headers = {};
  const rawHeaders = [];
  let previousHeaderName = null;
  try {
    for (const headerLine of headerLines) {
      if (headerLine.length === 0) {
        continue;
      }
      if (headerLine.startsWith(" ") || headerLine.startsWith("	")) {
        return {
          kind: "bad-request",
          closeConnection: true
        };
      }
      const separatorIndex = headerLine.indexOf(":");
      if (separatorIndex === -1) {
        return {
          kind: "bad-request",
          closeConnection: true
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
      closeConnection: true
    };
  }
  const requestMethod = requestMatch[1];
  const requestUrl = requestMatch[2];
  const httpMinorVersion = Number(requestMatch[4]);
  const requestCloseHeader = joinHeaderValue(headers.connection || "").toLowerCase();
  let closeConnection = httpMinorVersion === 0 ? !requestCloseHeader.includes("keep-alive") : requestCloseHeader.includes("close");
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
        bodyBase64: headerEnd + 4 < buffer.length ? buffer.subarray(headerEnd + 4).toString("base64") : void 0
      },
      upgradeHead: headerEnd + 4 < buffer.length ? buffer.subarray(headerEnd + 4) : Buffer.alloc(0)
    };
  }
  const transferEncoding = headers["transfer-encoding"];
  const contentLength = headers["content-length"];
  let requestBody = Buffer.alloc(0);
  let bytesConsumed = headerEnd + 4;
  if (transferEncoding !== void 0) {
    const tokens = splitTransferEncodingTokens(joinHeaderValue(transferEncoding));
    const chunkedCount = tokens.filter((entry) => entry === "chunked").length;
    const hasChunked = chunkedCount > 0;
    const chunkedIsFinal = hasChunked && tokens[tokens.length - 1] === "chunked";
    if (!hasChunked || chunkedCount !== 1 || !chunkedIsFinal || contentLength !== void 0) {
      return {
        kind: "bad-request",
        closeConnection: true
      };
    }
    const parsedChunked = parseChunkedBody(buffer.subarray(headerEnd + 4));
    if (parsedChunked === null) {
      return {
        kind: "bad-request",
        closeConnection: true
      };
    }
    if (!parsedChunked.complete) {
      return { kind: "incomplete" };
    }
    requestBody = parsedChunked.body;
    bytesConsumed = headerEnd + 4 + parsedChunked.bytesConsumed;
  } else if (contentLength !== void 0) {
    const parsedContentLength = parseContentLengthHeader(contentLength);
    if (parsedContentLength === null || parsedContentLength > MAX_HTTP_BODY_BYTES) {
      return {
        kind: "bad-request",
        closeConnection: true
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
      bodyBase64: requestBody.length > 0 ? requestBody.toString("base64") : void 0
    }
  };
}

function serializeRawHeaderPairs(rawHeaders, fallbackHeaders) {
  const headers = {};
  const rawNameMap = /* @__PURE__ */ new Map();
  const order = [];
  if (Array.isArray(rawHeaders) && rawHeaders.length > 0) {
    for (let index = 0; index < rawHeaders.length; index += 2) {
      const rawName = rawHeaders[index];
      const value = rawHeaders[index + 1];
      if (rawName === void 0 || value === void 0) {
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

function finalizeRawHeaderPairs(headers, rawNameMap, order) {
  const entries = [];
  const seen = /* @__PURE__ */ new Set();
  for (const key of order) {
    const value = headers[key];
    if (value === void 0) {
      continue;
    }
    const rawName = rawNameMap.get(key) || key;
    const serialized = Array.isArray(value) ? key === "set-cookie" ? value : [value.join(", ")] : [value];
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
    const serialized = Array.isArray(value) ? key === "set-cookie" ? value : [value.join(", ")] : [value];
    for (const entry of serialized) {
      entries.push([rawName, entry]);
    }
  }
  return entries;
}

function createBadRequestResponseBuffer() {
  return Buffer.from("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n", "latin1");
}

function serializeLoopbackResponse(response, request, requestWantsClose) {
  const statusCode = response.status || 200;
  const statusText = HTTP_STATUS_TEXT[statusCode] || "OK";
  const {
    headers,
    rawNameMap,
    order
  } = serializeRawHeaderPairs(response.rawHeaders, response.headers);
  const trailerInfo = serializeRawHeaderPairs(response.rawTrailers, response.trailers);
  const bodyBuffer = response.body == null ? Buffer.alloc(0) : response.bodyEncoding === "base64" ? Buffer.from(response.body, "base64") : Buffer.from(response.body, "utf8");
  const bodyAllowed = hasResponseBody(statusCode, request.method);
  const transferEncodingTokens = headers["transfer-encoding"] ? splitTransferEncodingTokens(joinHeaderValue(headers["transfer-encoding"])) : [];
  let isChunked = transferEncodingTokens.includes("chunked");
  const hasExplicitContentLength = headers["content-length"] !== void 0;
  let closeConnection = requestWantsClose || response.connectionEnded === true || response.connectionReset === true;
  if (!bodyAllowed) {
    if (isChunked) {
      closeConnection = true;
    }
    delete headers["content-length"];
  } else if (!isChunked && !hasExplicitContentLength) {
    if (response.streamed === true) {
      headers["transfer-encoding"] = "chunked";
      rawNameMap.set("transfer-encoding", "Transfer-Encoding");
      order.push("transfer-encoding");
      isChunked = true;
    } else {
      headers["content-length"] = String(bodyBuffer.length);
      rawNameMap.set("content-length", "Content-Length");
      order.push("content-length");
    }
  }
  if (closeConnection) {
    if (headers.connection === void 0) {
      headers.connection = "close";
      rawNameMap.set("connection", "Connection");
      order.push("connection");
    }
  } else if (headers.connection === void 0 && request.headers.connection !== void 0) {
    headers.connection = "keep-alive";
    rawNameMap.set("connection", "Connection");
    order.push("connection");
  }
  const serializedChunks = [];
  for (const informational of response.informational ?? []) {
    const infoHeaders = finalizeRawHeaderPairs(
      serializeRawHeaderPairs(informational.rawHeaders, informational.headers).headers,
      serializeRawHeaderPairs(informational.rawHeaders, informational.headers).rawNameMap,
      serializeRawHeaderPairs(informational.rawHeaders, informational.headers).order
    );
    const headerLines2 = infoHeaders.map(([name, value]) => `${name}: ${value}\r
`).join("");
    serializedChunks.push(
      Buffer.from(
        `HTTP/1.1 ${informational.status} ${informational.statusText || HTTP_STATUS_TEXT[informational.status] || ""}\r
${headerLines2}\r
`,
        "latin1"
      )
    );
  }
  const finalHeaders = finalizeRawHeaderPairs(headers, rawNameMap, order);
  const headerLines = finalHeaders.map(([name, value]) => `${name}: ${value}\r
`).join("");
  serializedChunks.push(
    Buffer.from(`HTTP/1.1 ${statusCode} ${statusText}\r
${headerLines}\r
`, "latin1")
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
          trailerInfo.order
        );
        for (const [name, value] of trailerPairs) {
          serializedChunks.push(Buffer.from(`${name}: ${value}\r
`, "latin1"));
        }
      }
      serializedChunks.push(Buffer.from("\r\n", "latin1"));
    } else if (bodyBuffer.length > 0) {
      serializedChunks.push(bodyBuffer);
    }
  }
  return {
    payload: serializedChunks.length === 1 ? serializedChunks[0] : Buffer.concat(serializedChunks),
    closeConnection
  };
}

var HTTP_STATUS_TEXT = {
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
  500: "Internal Server Error"
};

function isLoopbackRequestHost(hostname) {
  const bare = hostname.startsWith("[") && hostname.endsWith("]") ? hostname.slice(1, -1) : hostname;
  return bare === "localhost" || bare === "127.0.0.1" || bare === "::1";
}

var ServerIncomingMessage = class {
  headers;
  rawHeaders;
  method;
  url;
  socket;
  connection;
  rawBody;
  destroyed = false;
  errored;
  readable = true;
  httpVersion = "1.1";
  httpVersionMajor = 1;
  httpVersionMinor = 1;
  complete = true;
  aborted = false;
  // Readable stream state stub for frameworks that inspect internal state
  _readableState = { flowing: null, length: 0, ended: false, objectMode: false };
  _listeners = {};
  constructor(request) {
    this.headers = request.headers || {};
    this.rawHeaders = request.rawHeaders || [];
    if (!Array.isArray(this.rawHeaders) || this.rawHeaders.length % 2 !== 0) {
      this.rawHeaders = [];
    }
    this.method = request.method || "GET";
    this.url = request.url || "/";
    const fakeSocket = {
      encrypted: false,
      remoteAddress: "127.0.0.1",
      remotePort: 0,
      writable: true,
      on() {
        return fakeSocket;
      },
      once() {
        return fakeSocket;
      },
      removeListener() {
        return fakeSocket;
      },
      destroy() {
      },
      end() {
      }
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
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  once(event, listener) {
    const wrapped = (...args) => {
      this.off(event, wrapped);
      listener.call(this, ...args);
    };
    return this.on(event, wrapped);
  }
  off(event, listener) {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  emit(event, ...args) {
    const listeners = this._listeners[event];
    return dispatchCustomEmitterListeners(this, listeners, args);
  }
  // Readable stream stubs for framework compatibility
  unpipe() {
    return this;
  }
  pause() {
    return this;
  }
  resume() {
    return this;
  }
  read() {
    return null;
  }
  pipe(dest) {
    return dest;
  }
  isPaused() {
    return false;
  }
  setEncoding() {
    return this;
  }
  destroy(err) {
    this.destroyed = true;
    this.errored = err;
    if (err) {
      this.emit("error", err);
    }
    this.emit("close");
    return this;
  }
  _abort() {
    if (this.aborted) {
      return;
    }
    this.aborted = true;
    const error = createConnResetError("aborted");
    this.emit("aborted");
    this.emit("error", error);
    this.emit("close");
  }
};

var ServerResponseBridge = class {
  statusCode = 200;
  statusMessage = "OK";
  headersSent = false;
  writable = true;
  writableFinished = false;
  outputSize = 0;
  _headers = /* @__PURE__ */ new Map();
  _trailers = /* @__PURE__ */ new Map();
  _chunks = [];
  _chunksBytes = 0;
  _streamed = false;
  _listeners = {};
  _closedPromise;
  _resolveClosed = null;
  _connectionEnded = false;
  _connectionReset = false;
  _rawHeaderNames = /* @__PURE__ */ new Map();
  _rawTrailerNames = /* @__PURE__ */ new Map();
  _informational = [];
  _pendingRawInfoBuffer = "";
  _streamSocket = null;
  _streamRequest = null;
  _streamedDirectly = false;
  _streamHeadSent = false;
  _streamUsesChunked = false;
  _streamCloseConnection = false;
  constructor() {
    this._closedPromise = new Promise((resolve) => {
      this._resolveClosed = resolve;
    });
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  once(event, listener) {
    const wrapped = (...args) => {
      this.off(event, wrapped);
      listener.call(this, ...args);
    };
    return this.on(event, wrapped);
  }
  off(event, listener) {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  emit(event, ...args) {
    const listeners = this._listeners[event];
    if (!listeners || listeners.length === 0) return false;
    listeners.slice().forEach((fn) => fn.call(this, ...args));
    return true;
  }
  _emit(event, ...args) {
    this.emit(event, ...args);
  }
  writeHead(statusCode, headers) {
    if (statusCode >= 100 && statusCode < 200 && statusCode !== 101) {
      const informationalHeaders = /* @__PURE__ */ new Map();
      const informationalRawHeaderNames = /* @__PURE__ */ new Map();
      if (headers) {
        if (isFlatHeaderList(headers)) {
          for (let index = 0; index < headers.length; index += 2) {
            const key = headers[index];
            const value = headers[index + 1];
            if (key === void 0 || value === void 0) {
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
        return Array.isArray(serialized) ? serialized.map((entry) => [key, entry]) : [[key, serialized]];
      });
      const rawHeaders = Array.from(informationalHeaders.entries()).flatMap(([key, value]) => {
        const rawName = informationalRawHeaderNames.get(key) || key;
        const serialized = serializeHeaderValue(value);
        return Array.isArray(serialized) ? serialized.flatMap((entry) => [rawName, entry]) : [rawName, serialized];
      });
      this._informational.push({
        status: statusCode,
        statusText: HTTP_STATUS_TEXT[statusCode],
        headers: normalizedHeaders,
        rawHeaders
      });
      return this;
    }
    this.statusCode = statusCode;
    if (headers) {
      if (isFlatHeaderList(headers)) {
        for (let index = 0; index < headers.length; index += 2) {
          const key = headers[index];
          const value = headers[index + 1];
          if (key !== void 0 && value !== void 0) {
            this.setHeader(key, value);
          }
        }
      } else if (Array.isArray(headers)) {
        headers.forEach(([key, value]) => this.setHeader(key, value));
      } else {
        Object.entries(headers).forEach(
          ([key, value]) => this.setHeader(key, value)
        );
      }
    }
    this.headersSent = true;
    this.outputSize += 64;
    return this;
  }
  setHeader(name, value) {
    if (this.headersSent) {
      throw createErrorWithCode(
        "Cannot set headers after they are sent to the client",
        "ERR_HTTP_HEADERS_SENT"
      );
    }
    const lower = validateHeaderName(name).toLowerCase();
    validateHeaderValue(lower, value);
    const storedValue = Array.isArray(value) ? Array.from(value) : value;
    this._headers.set(lower, storedValue);
    if (!this._rawHeaderNames.has(lower)) {
      this._rawHeaderNames.set(lower, name);
    }
    return this;
  }
  setHeaders(headers) {
    if (this.headersSent) {
      throw createErrorWithCode(
        "Cannot set headers after they are sent to the client",
        "ERR_HTTP_HEADERS_SENT"
      );
    }
    if (!(headers instanceof Headers) && !(headers instanceof Map)) {
      throw createTypeErrorWithCode(
        `The "headers" argument must be an instance of Headers or Map. Received ${formatReceivedType(headers)}`,
        "ERR_INVALID_ARG_TYPE"
      );
    }
    if (headers instanceof Headers) {
      const pending = /* @__PURE__ */ Object.create(null);
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
  getHeader(name) {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE"
      );
    }
    const value = this._headers.get(name.toLowerCase());
    return value === void 0 ? void 0 : cloneStoredHeaderValue(value);
  }
  hasHeader(name) {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE"
      );
    }
    return this._headers.has(name.toLowerCase());
  }
  removeHeader(name) {
    if (typeof name !== "string") {
      throw createTypeErrorWithCode(
        `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
        "ERR_INVALID_ARG_TYPE"
      );
    }
    const lower = name.toLowerCase();
    this._headers.delete(lower);
    this._rawHeaderNames.delete(lower);
  }
  _appendChunk(chunk, encoding, streamed) {
    if (chunk == null) return true;
    const buf = typeof chunk === "string" ? Buffer.from(chunk, typeof encoding === "string" ? encoding : void 0) : chunk;
    if (this._chunksBytes + buf.byteLength > MAX_HTTP_BODY_BYTES) {
      throw new Error("ERR_HTTP_BODY_TOO_LARGE: response body exceeds " + MAX_HTTP_BODY_BYTES + " byte limit");
    }
    this._chunks.push(buf);
    this._chunksBytes += buf.byteLength;
    this._streamed ||= streamed;
    this.headersSent = true;
    this.outputSize += buf.byteLength;
    return true;
  }
  write(chunk, encodingOrCallback, callback) {
	if (this._streamSocket && !this.writableFinished) {
	  const buf = typeof chunk === "string" ? Buffer.from(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : void 0) : Buffer.from(chunk);
	  if (this._chunksBytes + buf.byteLength > MAX_HTTP_BODY_BYTES) {
		throw new Error("ERR_HTTP_BODY_TOO_LARGE: response body exceeds " + MAX_HTTP_BODY_BYTES + " byte limit");
	  }
	  this._chunksBytes += buf.byteLength;
	  this._streamed = true;
	  this.headersSent = true;
	  this.outputSize += buf.byteLength;
	  this._streamWriteHead();
	  if (!this._streamSocket.destroyed && buf.length > 0) {
		if (this._streamUsesChunked) {
		  this._streamSocket.write(Buffer.from(buf.length.toString(16) + "\r\n", "latin1"));
		  this._streamSocket.write(buf);
		  this._streamSocket.write(Buffer.from("\r\n", "latin1"));
		} else {
		  this._streamSocket.write(buf);
		}
	  }
	  const writeCallback = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
	  if (typeof writeCallback === "function") queueMicrotask(writeCallback);
	  return true;
	}
    this._appendChunk(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : void 0, true);
    const writeCallback = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    if (typeof writeCallback === "function") {
      queueMicrotask(writeCallback);
    }
    return true;
  }
  end(chunkOrCallback, encodingOrCallback, callback) {
    let chunk;
    let endCallback;
    if (typeof chunkOrCallback === "function") {
      endCallback = chunkOrCallback;
    } else {
      chunk = chunkOrCallback;
      endCallback = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
    }
    // Streaming fast path for socket-backed servers: a single `res.end(body)`
    // with no prior `res.write()` flushes headers then streams the body to the
    // connection socket in bounded slices. This avoids materializing the whole
    // body (plus its serialize/transmit copies) at once — a multi-MB response
    // otherwise trips the guest isolate heap-limit OOM guard before the host
    // can apply its own response-size limit. Per-slice `socket.destroyed`
    // checks also make the host's mid-stream rejection close graceful instead
    // of crashing the guest.
    if (
      this._streamSocket &&
      this._chunks.length === 0 &&
      !this.writableFinished &&
      !this._streamedDirectly &&
      !this._streamHeadSent
    ) {
      const encoding =
        typeof encodingOrCallback === "string" ? encodingOrCallback : void 0;
      this._streamEndBody(chunk, encoding);
      if (typeof endCallback === "function") {
        queueMicrotask(endCallback);
      }
      return this;
    }
	if (this._streamSocket && this._streamHeadSent && !this.writableFinished) {
	  if (chunk != null) this.write(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : void 0);
	  if (this._streamUsesChunked && !this._streamSocket.destroyed) {
		const trailers = [];
		for (const [key, value] of this._trailers) {
		  const rawName = this._rawTrailerNames.get(key) || key;
		  const serialized = serializeHeaderValue(value);
		  for (const entry of Array.isArray(serialized) ? serialized : [serialized]) {
			trailers.push(`${rawName}: ${entry}\r\n`);
		  }
		}
		this._streamSocket.write(Buffer.from(`0\r\n${trailers.join("")}\r\n`, "latin1"));
	  }
	  this._streamedDirectly = true;
	  this._finalize();
	  if (typeof endCallback === "function") queueMicrotask(endCallback);
	  return this;
	}
    if (chunk != null) {
      if (typeof chunk === "string" && typeof encodingOrCallback === "string") {
        this._appendChunk(chunk, encodingOrCallback, false);
      } else {
        this._appendChunk(chunk, void 0, false);
      }
    }
    this._finalize();
    if (typeof endCallback === "function") {
      queueMicrotask(endCallback);
    }
    return this;
  }
  _streamEndBody(body, encoding) {
    const isString = typeof body === "string";
    const SLICE_BYTES = 256 * 1024;
    // Compute the body byte length in bounded slices. `Buffer.byteLength` on a
    // whole multi-MB string allocates enough that, with the isolate already
    // near its heap cap, it trips the OOM guard before a single byte is sent.
    let byteLength = 0;
    if (body != null) {
      if (isString) {
        for (let offset = 0; offset < body.length; offset += SLICE_BYTES) {
          byteLength += Buffer.byteLength(
            body.slice(offset, offset + SLICE_BYTES),
            encoding,
          );
        }
      } else {
        byteLength = body.length;
      }
    }
    if (!this._headers.has("content-length") && !this._headers.has("transfer-encoding")) {
      this._headers.set("content-length", String(byteLength));
      this._rawHeaderNames.set("content-length", "Content-Length");
    }
    this.headersSent = true;
    // Serialize headers only (no buffered chunks => empty body in the payload),
    // then stream the real body separately in bounded slices.
    const headerResponse = this.serialize();
    const built = serializeLoopbackResponse(headerResponse, this._streamRequest, true);
    this._streamCloseConnection = built.closeConnection;
    this._streamedDirectly = true;
    this.outputSize += byteLength;
    if (!this._streamSocket.destroyed && built.payload.length > 0) {
      this._streamSocket.write(built.payload);
    }
    if (body != null && byteLength > 0) {
      if (isString) {
        for (let offset = 0; offset < body.length; offset += SLICE_BYTES) {
          if (this._streamSocket.destroyed) break;
          this._streamSocket.write(
            Buffer.from(body.slice(offset, offset + SLICE_BYTES), encoding),
          );
        }
      } else {
        for (let offset = 0; offset < body.length; offset += SLICE_BYTES) {
          if (this._streamSocket.destroyed) break;
          this._streamSocket.write(body.subarray(offset, offset + SLICE_BYTES));
        }
      }
    }
    this._finalize();
  }
	_streamWriteHead() {
	  if (this._streamHeadSent || !this._streamSocket || this._streamSocket.destroyed) return;
	  const hasContentLength = this._headers.has("content-length");
	  const transferEncoding = this._headers.get("transfer-encoding");
	  this._streamUsesChunked = !hasContentLength && (transferEncoding == null || String(transferEncoding).toLowerCase().includes("chunked"));
	  if (this._streamUsesChunked && transferEncoding == null) {
		this._headers.set("transfer-encoding", "chunked");
		this._rawHeaderNames.set("transfer-encoding", "Transfer-Encoding");
	  }
	  this._streamHeadSent = true;
	  const built = serializeLoopbackResponse(this.serialize(), this._streamRequest, true);
	  this._streamCloseConnection = built.closeConnection;
	  let payload = built.payload;
	  if (this._streamUsesChunked && payload.length >= 5 && payload.subarray(payload.length - 5).toString("latin1") === "0\r\n\r\n") {
		payload = payload.subarray(0, payload.length - 5);
	  }
	  if (payload.length > 0) this._streamSocket.write(payload);
	}
  getHeaderNames() {
    return Array.from(this._headers.keys());
  }
  getRawHeaderNames() {
    return Array.from(this._headers.keys()).map((key) => this._rawHeaderNames.get(key) || key);
  }
  getHeaders() {
    const result = /* @__PURE__ */ Object.create(null);
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
    write: (chunk, encodingOrCallback, callback) => {
      return this.write(chunk, encodingOrCallback, callback);
    }
  };
  connection = this.socket;
  // Node.js http.ServerResponse socket/stream compatibility stubs
  assignSocket() {
  }
  detachSocket() {
  }
  writeContinue() {
    this.writeHead(100);
  }
  writeProcessing() {
    this.writeHead(102);
  }
  addTrailers(headers) {
    if (Array.isArray(headers)) {
      for (let index = 0; index < headers.length; index += 2) {
        const key = headers[index];
        const value = headers[index + 1];
        if (key === void 0 || value === void 0) {
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
  cork() {
    this.socket.cork();
  }
  uncork() {
    this.socket.uncork();
  }
  setTimeout(_msecs) {
    return this;
  }
  get writableCorked() {
    return Number(this.socket.writableCorked || 0);
  }
  flushHeaders() {
    this.headersSent = true;
	this._streamWriteHead();
  }
  destroy(err) {
    this._connectionReset = true;
    if (err) {
      this._emit("error", err);
    }
    this._finalize();
  }
  async waitForClose() {
    await this._closedPromise;
  }
  serialize() {
    const bodyBuffer = this._chunks.length > 0 ? Buffer.concat(this._chunks) : Buffer.alloc(0);
    const serializedHeaders = Array.from(this._headers.entries()).flatMap(([key, value]) => {
      const serialized = serializeHeaderValue(value);
      if (Array.isArray(serialized)) {
        if (key === "set-cookie") {
          return serialized.map((entry) => [key, entry]);
        }
        return [[key, serialized.join(", ")]];
      }
      return [[key, serialized]];
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
      return Array.isArray(serialized) ? serialized.map((entry) => [key, entry]) : [[key, serialized]];
    });
    const rawTrailers = Array.from(this._trailers.entries()).flatMap(([key, value]) => {
      const rawName = this._rawTrailerNames.get(key) || key;
      const serialized = serializeHeaderValue(value);
      return Array.isArray(serialized) ? serialized.flatMap((entry) => [rawName, entry]) : [rawName, serialized];
    });
    return {
      status: this.statusCode,
      headers: serializedHeaders,
      rawHeaders,
      informational: this._informational.length > 0 ? [...this._informational] : void 0,
      body: bodyBuffer.toString("base64"),
      bodyEncoding: "base64",
      trailers: serializedTrailers.length > 0 ? serializedTrailers : void 0,
      rawTrailers: rawTrailers.length > 0 ? rawTrailers : void 0,
      connectionEnded: this._connectionEnded,
      connectionReset: this._connectionReset,
      streamed: this._streamed
    };
  }
  _writeRaw(chunk, callback) {
    this._pendingRawInfoBuffer += String(chunk);
    this._flushPendingRawInformational();
    if (typeof callback === "function") {
      queueMicrotask(callback);
    }
    return true;
  }
  _finalize() {
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
  _flushPendingRawInformational() {
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
        const headers = [];
        const rawHeaders = [];
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
          statusText: statusMatch[2] || HTTP_STATUS_TEXT[status] || void 0,
          headers,
          rawHeaders
        });
      }
      separatorIndex = this._pendingRawInfoBuffer.indexOf("\r\n\r\n");
    }
  }
};

var Server = class {
  listening = false;
  _listeners = {};
  _serverId;
  _netServer = null;
  _listenPromise = null;
  _address = null;
  _handleId = null;
  _hostCloseWaitStarted = false;
  _activeRequestDispatches = 0;
  _closePending = false;
  _closeRunning = false;
  _closeCallbacks = [];
  _tlsOptions = null;
  /** @internal Request listener stored on the instance (replaces serverRequestListeners Map). */
  _requestListener;
  constructor(requestListener, tlsOptions = null) {
    this._serverId = nextServerId++;
    this._requestListener = (...args) => {
      const listeners = this._listeners.request;
      if (!listeners || listeners.length === 0) return void 0;
      const results = listeners.slice().map((listener) => listener.call(this, ...args));
      return results.length === 1 ? results[0] : Promise.all(results);
    };
    if (requestListener) this.on("request", requestListener);
    this._tlsOptions = tlsOptions;
    serverInstances.set(this._serverId, this);
  }
  /** @internal Bridge-visible server ID for loopback self-dispatch. */
  get _bridgeServerId() {
    return this._serverId;
  }
  /** @internal Emit an event — used by upgrade dispatch to fire 'upgrade' events. */
  _emit(event, ...args) {
    const listeners = this._listeners[event];
    if (!listeners || listeners.length === 0) return;
    listeners.slice().forEach((listener) => listener.call(this, ...args));
  }
  _finishStart(resultJson) {
    const result = JSON.parse(resultJson);
    this._address = result.address;
    this.listening = true;
    this._handleId = `http-server:${this._serverId}`;
    debugBridgeNetwork("server listening", this._serverId, this._address);
    if (typeof _registerHandle === "function") {
      _registerHandle(this._handleId, "http server");
    }
    this._startHostCloseWait();
  }
  _completeClose() {
    this.listening = false;
    this._address = null;
    serverInstances.delete(this._serverId);
    if (this._handleId && typeof _unregisterHandle === "function") {
      _unregisterHandle(this._handleId);
    }
    this._handleId = null;
  }
  _beginRequestDispatch() {
    this._activeRequestDispatches += 1;
  }
  _endRequestDispatch() {
    this._activeRequestDispatches = Math.max(0, this._activeRequestDispatches - 1);
    if (this._closePending && this._activeRequestDispatches === 0) {
      this._closePending = false;
      queueMicrotask(() => {
        this._startClose();
      });
    }
  }
  _startHostCloseWait() {
    this._hostCloseWaitStarted = true;
  }
  async _start(port, hostname) {
    if (typeof NetServer === "undefined") {
      throw new Error(
        "http.createServer requires kernel-backed network bridge support"
      );
    }
    debugBridgeNetwork("server listen start", this._serverId, port, hostname);
    const netServer = new NetServer({ allowHalfOpen: true });
    this._netServer = netServer;
    netServer.on("connection", (socket) => {
      if (this._tlsOptions) {
        const tlsSocket = new TLSSocket(socket, {
          ...this._tlsOptions,
          isServer: true
        });
        tlsSocket.server = this;
        tlsSocket.once("secure", () => {
          this._emit("secureConnection", tlsSocket);
          this._emit("connection", tlsSocket);
          attachHttpServerSocket(this, tlsSocket);
        });
        tlsSocket.on("error", (error) => {
          this._emit("tlsClientError", error, tlsSocket);
        });
        return;
      }
      this._emit("connection", socket);
      attachHttpServerSocket(this, socket);
    });
    netServer.on("error", (error) => {
      this._emit("error", error);
    });
    await new Promise((resolve, reject) => {
      let settled = false;
      const cleanup = () => {
        netServer.removeListener?.("listening", onListening);
        netServer.removeListener?.("error", onError);
      };
      const onListening = () => {
        if (settled) return;
        settled = true;
        cleanup();
        resolve();
      };
      const onError = (error) => {
        if (settled) return;
        settled = true;
        cleanup();
        reject(error instanceof Error ? error : new Error(String(error)));
      };
      netServer.once("listening", onListening);
      netServer.once("error", onError);
      netServer.listen(port ?? 0, hostname);
    });
    this._address = netServer.address();
    this.listening = true;
    this._startHostCloseWait();
    debugBridgeNetwork("server listening", this._serverId, this._address);
  }
  listen(portOrCb, hostOrCb, cb) {
    const port = typeof portOrCb === "number" ? portOrCb : void 0;
    const hostname = typeof hostOrCb === "string" ? hostOrCb : void 0;
    const callback = typeof cb === "function" ? cb : typeof hostOrCb === "function" ? hostOrCb : typeof portOrCb === "function" ? portOrCb : void 0;
    if (!this._listenPromise) {
      this._listenPromise = this._start(port, hostname).then(() => {
        this._emit("listening");
        callback?.call(this);
      }).catch((error) => {
        this._emit("error", error);
      });
    }
    return this;
  }
  close(cb) {
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
  _startClose() {
    if (this._closeRunning) {
      return;
    }
    this._closeRunning = true;
    const run = async () => {
      try {
        if (this._listenPromise) {
          await this._listenPromise;
        }
        const netServer = this._netServer;
        if (this.listening && netServer) {
          debugBridgeNetwork("server close net server", this._serverId);
          await new Promise((resolve, reject) => {
            netServer.close((error) => {
              if (error) {
                reject(error);
              } else {
                resolve();
              }
            });
          });
        }
        this._netServer = null;
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
  address() {
    return this._address;
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  once(event, listener) {
    const wrapped = (...args) => {
      this.off(event, wrapped);
      listener.call(this, ...args);
    };
    return this.on(event, wrapped);
  }
  off(event, listener) {
    const listeners = this._listeners[event];
    if (!listeners) return this;
    const index = listeners.indexOf(listener);
    if (index !== -1) listeners.splice(index, 1);
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  removeAllListeners(event) {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }
  listenerCount(event) {
    return this._listeners[event]?.length || 0;
  }
  listeners(event) {
    return [...this._listeners[event] || []];
  }
  emit(event, ...args) {
    this._emit(event, ...args);
    return this.listenerCount(event) > 0;
  }
  // Node.js Server timeout properties (no-op in sandbox)
  keepAliveTimeout = 5e3;
  requestTimeout = 3e5;
  headersTimeout = 6e4;
  timeout = 0;
  maxRequestsPerSocket = 0;
  setTimeout(_msecs, _callback) {
    if (typeof _msecs === "number") this.timeout = _msecs;
    return this;
  }
  ref() {
    return this;
  }
  unref() {
    return this;
  }
};

function ServerCallable(requestListener) {
  return new Server(requestListener);
}

ServerCallable.prototype = Server.prototype;

async function dispatchServerRequest(serverId, requestJson) {
  const server = serverInstances.get(serverId);
  if (!server) {
    throw new Error(`Unknown HTTP server: ${serverId}`);
  }
  const listener = server._requestListener;
  server._beginRequestDispatch();
  const request = JSON.parse(requestJson);
  const incoming = new ServerIncomingMessage(request);
  const outgoing = new ServerResponseBridge();
  incoming.socket = outgoing.socket;
  incoming.connection = outgoing.socket;
  const pendingImmediates = [];
  const pendingTimers = [];
  const trackedTimers = /* @__PURE__ */ new Map();
  let consumedTimerCount = 0;
  let consumedImmediateCount = 0;
  try {
    try {
      const originalSetImmediate = globalThis.setImmediate;
      const originalSetTimeout = globalThis.setTimeout;
      const originalClearTimeout = globalThis.clearTimeout;
      if (typeof originalSetImmediate === "function") {
        globalThis.setImmediate = ((callback, ...args) => {
          const pending = new Promise((resolve) => {
            queueMicrotask(() => {
              try {
                callback(...args);
              } finally {
                resolve();
              }
            });
          });
          pendingImmediates.push(pending);
          return 0;
        });
      }
      if (typeof originalSetTimeout === "function") {
        globalThis.setTimeout = ((callback, delay, ...args) => {
          if (typeof callback !== "function") {
            return originalSetTimeout(callback, delay, ...args);
          }
          const normalizedDelay = typeof delay === "number" && Number.isFinite(delay) ? Math.max(0, delay) : 0;
          if (normalizedDelay > 1e3) {
            return originalSetTimeout(callback, normalizedDelay, ...args);
          }
          let resolvePending;
          const pending = new Promise((resolve) => {
            resolvePending = resolve;
          });
          let handle;
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
        });
      }
      if (typeof originalClearTimeout === "function") {
        globalThis.clearTimeout = ((handle) => {
          if (handle != null) {
            const resolvePending = trackedTimers.get(handle);
            if (resolvePending) {
              trackedTimers.delete(handle);
              resolvePending();
            }
          }
          return originalClearTimeout(handle);
        });
      }
      try {
        const listenerResult = listener(incoming, outgoing);
        if (incoming.rawBody && incoming.rawBody.length > 0) {
          incoming.emit("data", incoming.rawBody);
        }
        incoming.emit("end");
        await Promise.resolve(listenerResult);
        while (consumedTimerCount < pendingTimers.length || consumedImmediateCount < pendingImmediates.length) {
          const pending = [
            ...pendingTimers.slice(consumedTimerCount),
            ...pendingImmediates.slice(consumedImmediateCount)
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
    return JSON.stringify(outgoing.serialize());
  } finally {
    server._endRequestDispatch();
  }
}

async function dispatchHttp2CompatibilityRequest(serverId, requestId) {
  const pending = pendingHttp2CompatRequests.get(requestId);
  if (!pending || pending.serverId !== serverId || typeof _networkHttp2ServerRespondRaw === "undefined") {
    return;
  }
  pendingHttp2CompatRequests.delete(requestId);
  const server = http2Servers.get(serverId);
  if (!server) {
    _networkHttp2ServerRespondRaw.applySync(void 0, [
      serverId,
      requestId,
      JSON.stringify({
        status: 500,
        headers: [["content-type", "text/plain"]],
        body: "Unknown HTTP/2 server",
        bodyEncoding: "utf8"
      })
    ]);
    return;
  }
  const request = JSON.parse(pending.requestJson);
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
    _networkHttp2ServerRespondRaw.applySync(void 0, [
      serverId,
      requestId,
      JSON.stringify(outgoing.serialize())
    ]);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    _networkHttp2ServerRespondRaw.applySync(void 0, [
      serverId,
      requestId,
      JSON.stringify({
        status: 500,
        headers: [["content-type", "text/plain"]],
        body: `Error: ${message}`,
        bodyEncoding: "utf8"
      })
    ]);
  }
}

async function dispatchLoopbackServerRequest(serverOrId, requestInput) {
  const server = typeof serverOrId === "number" ? serverInstances.get(serverOrId) : serverOrId;
  if (!server) {
    throw new Error(
      `Unknown HTTP server: ${typeof serverOrId === "number" ? serverOrId : "<detached>"}`
    );
  }
  const request = typeof requestInput === "string" ? JSON.parse(requestInput) : requestInput;
  const incoming = new ServerIncomingMessage(request);
  const outgoing = new ServerResponseBridge();
  incoming.socket = outgoing.socket;
  incoming.connection = outgoing.socket;
  const pendingImmediates = [];
  const pendingTimers = [];
  const trackedTimers = /* @__PURE__ */ new Map();
  let consumedTimerCount = 0;
  let consumedImmediateCount = 0;
  server._beginRequestDispatch();
  try {
    try {
      const originalSetImmediate = globalThis.setImmediate;
      const originalSetTimeout = globalThis.setTimeout;
      const originalClearTimeout = globalThis.clearTimeout;
      if (typeof originalSetImmediate === "function") {
        globalThis.setImmediate = ((callback, ...args) => {
          const pending = new Promise((resolve) => {
            queueMicrotask(() => {
              try {
                callback(...args);
              } finally {
                resolve();
              }
            });
          });
          pendingImmediates.push(pending);
          return 0;
        });
      }
      if (typeof originalSetTimeout === "function") {
        globalThis.setTimeout = ((callback, delay, ...args) => {
          if (typeof callback !== "function") {
            return originalSetTimeout(callback, delay, ...args);
          }
          const normalizedDelay = typeof delay === "number" && Number.isFinite(delay) ? Math.max(0, delay) : 0;
          if (normalizedDelay > 1e3) {
            return originalSetTimeout(callback, normalizedDelay, ...args);
          }
          let resolvePending;
          const pending = new Promise((resolve) => {
            resolvePending = resolve;
          });
          let handle;
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
        });
      }
      if (typeof originalClearTimeout === "function") {
        globalThis.clearTimeout = ((handle) => {
          if (handle != null) {
            const resolvePending = trackedTimers.get(handle);
            if (resolvePending) {
              trackedTimers.delete(handle);
              resolvePending();
            }
          }
          return originalClearTimeout(handle);
        });
      }
      try {
        const listenerResult = server._requestListener(incoming, outgoing);
        if (incoming.rawBody && incoming.rawBody.length > 0) {
          incoming.emit("data", incoming.rawBody);
        }
        incoming.emit("end");
        await Promise.resolve(listenerResult);
        while (consumedTimerCount < pendingTimers.length || consumedImmediateCount < pendingImmediates.length) {
          const pending = [
            ...pendingTimers.slice(consumedTimerCount),
            ...pendingImmediates.slice(consumedImmediateCount)
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
      }
    };
  } finally {
    server._endRequestDispatch();
  }
}

async function dispatchSocketBackedServerRequest(server, requestInput, streamSocket) {
  const request = typeof requestInput === "string" ? JSON.parse(requestInput) : requestInput;
  const incoming = new ServerIncomingMessage(request);
  const outgoing = new ServerResponseBridge();
  incoming.socket = outgoing.socket;
  incoming.connection = outgoing.socket;
  // Enable the streaming fast path so a single large `res.end(body)` is written
  // to the connection socket in slices instead of buffered + serialized whole.
  if (streamSocket) {
    outgoing._streamSocket = streamSocket;
    outgoing._streamRequest = request;
  }
  server._beginRequestDispatch();
  try {
    try {
      const listenerResult = server._requestListener(incoming, outgoing);
      if (incoming.rawBody && incoming.rawBody.length > 0) {
        incoming.emit("data", incoming.rawBody);
      }
      incoming.emit("end");
      await Promise.resolve(listenerResult);
    } catch (err) {
      outgoing.statusCode = 500;
      try {
        outgoing.end(err instanceof Error ? `Error: ${err.message}` : "Error");
      } catch {
        if (!outgoing.writableFinished) outgoing.end();
      }
    }
    // A Node request listener is callback-driven: frameworks such as Fastify
    // return `undefined`, then finish the response after an awaited route hook.
    // Ending here as soon as the listener returns races that continuation and
    // produces a synthetic empty 200 response. Leave the request open until
    // ServerResponse.end()/destroy() closes it, matching native Node.
    await outgoing.waitForClose();
    let aborted = false;
    const abortRequest = () => {
      if (aborted) {
        return;
      }
      aborted = true;
      incoming._abort();
    };
    if (outgoing._streamedDirectly) {
      // Response already written straight to the socket; nothing left to serialize.
      return {
        streamedDirectly: true,
        closeConnection: outgoing._streamCloseConnection,
        abortRequest
      };
    }
    return {
      responseJson: JSON.stringify(outgoing.serialize()),
      abortRequest
    };
  } finally {
    server._endRequestDispatch();
  }
}

function attachHttpServerSocket(server, socket) {
  let buffer = Buffer.alloc(0);
  let dispatchRunning = false;
  let dispatchPending = false;
  let ended = false;
  let detached = false;
  const cleanup = () => {
    if (detached) {
      return;
    }
    detached = true;
    socket.off?.("data", onData);
    socket.removeListener?.("data", onData);
    socket.off?.("end", onEnd);
    socket.removeListener?.("end", onEnd);
    socket.off?.("close", onClose);
    socket.removeListener?.("close", onClose);
    socket.off?.("error", onError);
    socket.removeListener?.("error", onError);
  };
  const scheduleDispatch = () => {
    if (dispatchRunning) {
      dispatchPending = true;
      return;
    }
    dispatchRunning = true;
    void processRequests().finally(() => {
      dispatchRunning = false;
      if (dispatchPending && !detached) {
        dispatchPending = false;
        scheduleDispatch();
      } else {
        dispatchPending = false;
      }
    });
  };
  const finishSocket = () => {
    cleanup();
    if (!socket.destroyed && !socket._writableEnded) {
      socket.end();
    }
  };
  const onData = (chunk) => {
    const payload = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    buffer = buffer.length === 0 ? payload : Buffer.concat([buffer, payload]);
    scheduleDispatch();
  };
  const onEnd = () => {
    ended = true;
    if (buffer.length === 0) {
      cleanup();
      return;
    }
    scheduleDispatch();
  };
  const onClose = () => {
    cleanup();
  };
  const onError = () => {
    cleanup();
  };
  async function processRequests() {
    let closeAfterDrain = false;
    while (!detached && !socket.destroyed) {
      const parsed = parseLoopbackRequestBuffer(buffer, server);
      if (parsed.kind === "incomplete") {
        if (ended && buffer.length > 0) {
          socket.write(createBadRequestResponseBuffer());
          finishSocket();
        }
        return;
      }
      if (parsed.kind === "bad-request") {
        socket.write(createBadRequestResponseBuffer());
        finishSocket();
        buffer = Buffer.alloc(0);
        return;
      }
      buffer = buffer.subarray(parsed.bytesConsumed);
		if (parsed.upgradeHead) {
			cleanup();
			const incoming = new ServerIncomingMessage(parsed.request);
			incoming.socket = socket;
			incoming.connection = socket;
			try {
				server._emit("upgrade", incoming, socket, parsed.upgradeHead);
			} catch (error) {
				// EventEmitter listener failures are uncaught in Node. Do not turn an
				// upgrade-handler exception into a silent socket close or a dangling
				// handshake merely because request dispatch runs in an async pump.
				queueMicrotask(() => {
					throw error;
				});
			}
			return;
		}
      const result = await dispatchSocketBackedServerRequest(
        server,
        parsed.request,
        socket,
      );
      if (detached || socket.destroyed) {
        return;
      }
      // Keep-alive for socket-backed HTTP servers is intentionally deferred:
      // pipelined bytes already in `buffer` drain, then this connection closes.
      // Revisit when the bridge owns full Node-compatible request lifecycle
      // timers and per-socket request limits.
      let mustClose;
      if (result.streamedDirectly) {
        // Response was already streamed straight to the socket by res.end().
        mustClose = result.closeConnection;
      } else {
        const response = JSON.parse(result.responseJson);
        const serialized = serializeLoopbackResponse(response, parsed.request, true);
        if (!closeAfterDrain && serialized.payload.length > 0) {
          socket.write(serialized.payload);
        }
        mustClose = serialized.closeConnection;
      }
      if (mustClose) {
        closeAfterDrain = true;
        if (buffer.length === 0) {
          finishSocket();
          return;
        }
      }
    }
  }
  socket.on("data", onData);
  socket.once("end", onEnd);
  socket.once("close", onClose);
  socket.once("error", onError);
}

function dispatchSocketRequest(event, serverId, requestJson, headBase64, socketId) {
  const server = serverInstances.get(serverId);
  if (!server) {
    throw new Error(`Unknown HTTP server for ${event}: ${serverId}`);
  }
  const request = JSON.parse(requestJson);
  const incoming = new ServerIncomingMessage(request);
  const head = typeof Buffer !== "undefined" ? Buffer.from(headBase64, "base64") : new Uint8Array(0);
  const hostHeader = incoming.headers["host"];
  const socket = new UpgradeSocket(socketId, {
    host: (Array.isArray(hostHeader) ? hostHeader[0] : hostHeader)?.split(":")[0] || "127.0.0.1"
  });
  upgradeSocketInstances.set(socketId, socket);
  server._emit(event, incoming, socket, head);
}

var upgradeSocketInstances = /* @__PURE__ */ new Map();

var UpgradeSocket = class {
  remoteAddress;
  remotePort;
  localAddress = "127.0.0.1";
  localPort = 0;
  connecting = false;
  destroyed = false;
  writable = true;
  readable = true;
  readyState = "open";
  bytesWritten = 0;
  _listeners = {};
  _socketId;
  // Readable stream state stub for ws compatibility (socketOnClose checks _readableState.endEmitted)
  _readableState = { endEmitted: false, ended: false };
  _writableState = { finished: false, errorEmitted: false };
  constructor(socketId, options) {
    this._socketId = socketId;
    this.remoteAddress = options?.host || "127.0.0.1";
    this.remotePort = options?.port || 80;
  }
  setTimeout(_ms, _cb) {
    return this;
  }
  setNoDelay(_noDelay) {
    return this;
  }
  setKeepAlive(_enable, _delay) {
    return this;
  }
  ref() {
    return this;
  }
  unref() {
    return this;
  }
  cork() {
  }
  uncork() {
  }
  pause() {
    return this;
  }
  resume() {
    return this;
  }
  address() {
    return { address: this.localAddress, family: "IPv4", port: this.localPort };
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  addListener(event, listener) {
    return this.on(event, listener);
  }
  once(event, listener) {
    const wrapper = (...args) => {
      this.off(event, wrapper);
      listener(...args);
    };
    return this.on(event, wrapper);
  }
  off(event, listener) {
    if (this._listeners[event]) {
      const idx = this._listeners[event].indexOf(listener);
      if (idx !== -1) this._listeners[event].splice(idx, 1);
    }
    return this;
  }
  removeListener(event, listener) {
    return this.off(event, listener);
  }
  removeAllListeners(event) {
    if (event) {
      delete this._listeners[event];
    } else {
      this._listeners = {};
    }
    return this;
  }
  emit(event, ...args) {
    const handlers = this._listeners[event];
    return dispatchCustomEmitterListeners(this, handlers, args);
  }
  listenerCount(event) {
    return this._listeners[event]?.length || 0;
  }
  write(data, encodingOrCb, cb) {
    if (this.destroyed) return false;
    const callback = typeof encodingOrCb === "function" ? encodingOrCb : cb;
    if (typeof _upgradeSocketWriteRaw !== "undefined") {
      let base64;
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
      _upgradeSocketWriteRaw.applySync(void 0, [this._socketId, base64]);
    }
    if (callback) callback();
    return true;
  }
  end(data) {
    if (data) this.write(data);
    if (typeof _upgradeSocketEndRaw !== "undefined" && !this.destroyed) {
      _upgradeSocketEndRaw.applySync(void 0, [this._socketId]);
    }
    this.writable = false;
    this.emit("finish");
    return this;
  }
  destroy(err) {
    if (this.destroyed) return this;
    this.destroyed = true;
    this.writable = false;
    this.readable = false;
    this._readableState.endEmitted = true;
    this._readableState.ended = true;
    this._writableState.finished = true;
    if (typeof _upgradeSocketDestroyRaw !== "undefined") {
      _upgradeSocketDestroyRaw.applySync(void 0, [this._socketId]);
    }
    upgradeSocketInstances.delete(this._socketId);
    if (err) this.emit("error", err);
    this.emit("close", false);
    return this;
  }
  // Push data received from the host into this socket
  _pushData(data) {
    this.emit("data", data);
  }
  // Signal end-of-stream from the host
  _pushEnd() {
    this.readable = false;
    this._readableState.endEmitted = true;
    this._readableState.ended = true;
    this._writableState.finished = true;
    this.emit("end");
    this.emit("close", false);
    upgradeSocketInstances.delete(this._socketId);
  }
};

function dispatchUpgradeRequest(serverId, requestJson, headBase64, socketId) {
  dispatchSocketRequest("upgrade", serverId, requestJson, headBase64, socketId);
}

function dispatchConnectRequest(serverId, requestJson, headBase64, socketId) {
  dispatchSocketRequest("connect", serverId, requestJson, headBase64, socketId);
}

function onUpgradeSocketData(socketId, dataBase64) {
  const socket = upgradeSocketInstances.get(socketId);
  if (socket) {
    const data = typeof Buffer !== "undefined" ? Buffer.from(dataBase64, "base64") : new Uint8Array(0);
    socket._pushData(data);
  }
}

function onUpgradeSocketEnd(socketId) {
  const socket = upgradeSocketInstances.get(socketId);
  if (socket) {
    socket._pushEnd();
  }
}

function ServerResponseCallable() {
  this.statusCode = 200;
  this.statusMessage = "OK";
  this.headersSent = false;
  this.writable = true;
  this.writableFinished = false;
  this.outputSize = 0;
  this._headers = /* @__PURE__ */ new Map();
  this._trailers = /* @__PURE__ */ new Map();
  this._rawHeaderNames = /* @__PURE__ */ new Map();
  this._rawTrailerNames = /* @__PURE__ */ new Map();
  this._informational = [];
  this._pendingRawInfoBuffer = "";
  this._chunks = [];
  this._chunksBytes = 0;
  this._listeners = {};
  this._closedPromise = new Promise((resolve) => {
    this._resolveClosed = resolve;
  });
  this._connectionEnded = false;
  this._connectionReset = false;
  this._writableState = { length: 0, ended: false, finished: false, objectMode: false, corked: 0 };
  const fakeSocket = {
    writable: true,
    writableCorked: 0,
    writableHighWaterMark: 16 * 1024,
    on() {
      return fakeSocket;
    },
    once() {
      return fakeSocket;
    },
    removeListener() {
      return fakeSocket;
    },
    destroy() {
    },
    end() {
    },
    cork() {
    },
    uncork() {
    },
    write: (chunk, encodingOrCallback, callback) => {
      return this.write(chunk, encodingOrCallback, callback);
    }
  };
  this.socket = fakeSocket;
  this.connection = fakeSocket;
}

ServerResponseCallable.prototype = Object.create(ServerResponseBridge.prototype, {
  constructor: { value: ServerResponseCallable, writable: true, configurable: true }
});

function createHttpModule(protocol) {
  const defaultProtocol = protocol === "https" ? "https:" : "http:";
  const moduleAgent = new Agent({
    keepAlive: false,
    createConnection(options, cb) {
      return createHttpRequestSocket({ ...options, protocol: defaultProtocol }, cb);
    }
  });
  function ensureProtocol(opts) {
    if (!opts.protocol) return { ...opts, protocol: defaultProtocol };
    return opts;
  }
  function withModuleDefaultAgent(opts) {
    if (opts.agent !== void 0) {
      return opts;
    }
    return {
      ...opts,
      _agentOSDefaultAgent: moduleAgent
    };
  }
  return {
    request(options, optionsOrCallback, maybeCallback) {
      let opts;
      const callback = typeof optionsOrCallback === "function" ? optionsOrCallback : maybeCallback;
      if (typeof options === "string") {
        const url = new URL(options);
        opts = {
          protocol: url.protocol,
          hostname: url.hostname,
          port: url.port,
          path: url.pathname + url.search,
          ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
        };
      } else if (options instanceof URL) {
        opts = {
          protocol: options.protocol,
          hostname: options.hostname,
          port: options.port,
          path: options.pathname + options.search,
          ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
        };
      } else {
        opts = {
          ...options,
          ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
        };
      }
      return new ClientRequest(withModuleDefaultAgent(ensureProtocol(opts)), callback);
    },
    get(options, optionsOrCallback, maybeCallback) {
      let opts;
      const callback = typeof optionsOrCallback === "function" ? optionsOrCallback : maybeCallback;
      if (typeof options === "string") {
        const url = new URL(options);
        opts = {
          protocol: url.protocol,
          hostname: url.hostname,
          port: url.port,
          path: url.pathname + url.search,
          method: "GET",
          ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
        };
      } else if (options instanceof URL) {
        opts = {
          protocol: options.protocol,
          hostname: options.hostname,
          port: options.port,
          path: options.pathname + options.search,
          method: "GET",
          ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
        };
      } else {
        opts = {
          ...options,
          ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {},
          method: "GET"
        };
      }
      const req = new ClientRequest(withModuleDefaultAgent(ensureProtocol(opts)), callback);
      req.end();
      return req;
    },
    createServer(_optionsOrListener, maybeListener) {
      const listener = typeof _optionsOrListener === "function" ? _optionsOrListener : maybeListener;
      const serverOptions = typeof _optionsOrListener === "function" ? null : _optionsOrListener;
      return new Server(listener, protocol === "https" ? serverOptions : null);
    },
    Agent,
    globalAgent: moduleAgent,
    Server: ServerCallable,
    ServerResponse: ServerResponseCallable,
    IncomingMessage,
    ClientRequest,
    validateHeaderName,
    validateHeaderValue,
    _checkIsHttpToken: checkIsHttpToken,
    _checkInvalidHeaderChar: checkInvalidHeaderChar,
    maxHeaderSize: 65535,
    METHODS: [...HTTP_METHODS],
    STATUS_CODES: HTTP_STATUS_TEXT
  };
}

var http = createHttpModule("http");

exposeCustomGlobal("_httpModule", http);

exposeCustomGlobal("_dnsModule", dns);

function onHttpServerRequest(eventType, payload) {
  debugBridgeNetwork("http stream event", eventType, payload);
  if (eventType !== "http_request") {
    return;
  }
  if (!payload || payload.serverId === void 0 || payload.requestId === void 0 || typeof payload.request !== "string") {
    return;
  }
  if (typeof _networkHttpServerRespondRaw === "undefined") {
    debugBridgeNetwork("http stream missing respond bridge");
    return;
  }
  void dispatchServerRequest(payload.serverId, payload.request).then((responseJson) => {
    debugBridgeNetwork("http stream response", payload.serverId, payload.requestId);
    _networkHttpServerRespondRaw.applySync(void 0, [
      payload.serverId,
      payload.requestId,
      responseJson
    ]);
  }).catch((err) => {
    const message = err instanceof Error ? err.message : String(err);
    debugBridgeNetwork("http stream error", payload.serverId, payload.requestId, message);
    _networkHttpServerRespondRaw.applySync(void 0, [
      payload.serverId,
      payload.requestId,
      JSON.stringify({
        status: 500,
        headers: [["content-type", "text/plain"]],
        body: `Error: ${message}`,
        bodyEncoding: "utf8"
      })
    ]);
  });
}

exposeCustomGlobal("_httpServerDispatch", onHttpServerRequest);

exposeCustomGlobal("_httpServerUpgradeDispatch", dispatchUpgradeRequest);

exposeCustomGlobal("_httpServerConnectDispatch", dispatchConnectRequest);

exposeCustomGlobal("_http2Dispatch", onHttp2Dispatch);

exposeCustomGlobal("_upgradeSocketData", onUpgradeSocketData);
var https = createHttpModule("https");

exposeCustomGlobal("_httpsModule", https);
export { Agent, ClientRequest, DirectTunnelSocket, FakeSocket, HTTP_METHODS, HTTP_STATUS_TEXT, HTTP_TOKEN_EXTRA_CHARS, INVALID_REQUEST_PATH_REGEXP, IncomingMessage, Server, ServerCallable, ServerIncomingMessage, ServerResponseBridge, ServerResponseCallable, UpgradeSocket, appendNormalizedHeader, attachHttpServerSocket, buildHostHeader, buildRawHttpHeaderPairs, buildUndiciOrigin, checkInvalidHeaderChar, checkIsHttpToken, cloneStoredHeaderValue, createAbortError2, createBadRequestResponseBuffer, createConnResetError, createErrorWithCode, createHttpModule, createHttpRequestSocket, createInvalidArgTypeError2, createTypeErrorWithCode, createUnsupportedHttpSocketWriteError, debugBridgeNetwork, dispatchConnectRequest, dispatchHttp2CompatibilityRequest, dispatchLoopbackServerRequest, dispatchServerRequest, dispatchSocketBackedServerRequest, dispatchSocketRequest, dispatchUpgradeRequest, finalizeRawHeaderPairs, flattenHeaderPairs, formatReceivedType, getUndiciClientForSocket, hasResponseBody, hasUpgradeRequestHeaders, http, https, isFlatHeaderList, isLoopbackRequestHost, isRawSocketRequest, isSocketReadyForProtocol, joinHeaderValue, nextServerId, normalizeRequestHeaders, normalizeSocketChunk, onHttpServerRequest, onUpgradeSocketData, onUpgradeSocketEnd, parseChunkedBody, parseContentLengthHeader, parseLoopbackRequestBuffer, parseRawHttpResponse, readUndiciReadableBody, serializeHeaderValue, serializeLoopbackResponse, serializeRawHeaderPairs, serializeRawHttpRequest, serverInstances, socketReadyEventNameForProtocol, splitTransferEncodingTokens, upgradeSocketInstances, validateHeaderName, validateHeaderValue, validateRequestMethod, validateRequestPath, waitForRawHttpResponse, waitForRawHttpResponseHead, waitForSocketReadyForProtocol };
