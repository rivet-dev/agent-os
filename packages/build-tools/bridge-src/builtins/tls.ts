import { exposeCustomGlobal } from "../global-exposure.js";
import { NetServer, NetSocket, buildSerializedTlsOptions, isTlsSecureContextWrapper, netModule, parseTlsClientHello } from "./net.js";

function createSecureContextWrapper(options) {
  return {
    __agentOsTlsContext: buildSerializedTlsOptions(options),
    context: {}
  };
}

function adoptRawTlsSocket(rawSocket, options) {
  if (!(rawSocket instanceof NetSocket)) {
    throw new TypeError("tls.TLSSocket requires a net.Socket instance");
  }
  const normalizedOptions = options && typeof options === "object" ? { ...options } : {};
  Object.setPrototypeOf(rawSocket, TLSSocket.prototype);
  const upgradeOptions = buildSerializedTlsOptions(
    normalizedOptions,
    {
      isServer: normalizedOptions.isServer === true,
      servername: normalizedOptions.servername ?? rawSocket.servername ?? rawSocket.remoteAddress ?? "127.0.0.1"
    }
  );
  if (!upgradeOptions.isServer) {
    rawSocket.servername = upgradeOptions.servername;
  }
  if (rawSocket._connected) {
    rawSocket._upgradeTls(upgradeOptions);
  } else {
    rawSocket.once("connect", () => {
      rawSocket._upgradeTls(upgradeOptions);
    });
  }
  return rawSocket;
}

class TLSSocket extends NetSocket {
  constructor(socketOrOptions, options) {
    if (socketOrOptions instanceof NetSocket) {
      super({ allowHalfOpen: socketOrOptions.allowHalfOpen === true });
      const adopted = adoptRawTlsSocket(socketOrOptions, options);
      adopted._handle ??= { _parentWrap: { constructor: NetSocket } };
      return adopted;
    }
    super(
      socketOrOptions && typeof socketOrOptions === "object" ? socketOrOptions : options
    );
    // http2-wrapper feature-detects Node's internal JSStreamSocket constructor
    // through this shape at module load. Preserve the shape without exposing a
    // host descriptor; proxy transports still flow through AgentOS NetSocket.
    this._handle ??= { _parentWrap: { constructor: NetSocket } };
  }
}

function tlsConnect(...args) {
  let socket;
  let options;
  const values = [...args];
  const cb = typeof values[values.length - 1] === "function" ? values.pop() : void 0;
  if (values[0] != null && typeof values[0] === "object") {
    options = { ...values[0] };
    if (options.socket) {
      socket = options.socket;
		} else {
			socket = new NetSocket();
			socket.connect({
				host: options.host ?? "127.0.0.1",
				port: options.port,
				localAddress: options.localAddress,
				localPort: options.localPort
			});
		}
  } else {
    const positional = {};
    if (values.length > 0) {
      positional.port = values.shift();
    }
    if (typeof values[0] === "string") {
      positional.host = values.shift();
    }
    const providedOptions = values[0] != null && typeof values[0] === "object" ? { ...values[0] } : {};
    options = { ...providedOptions, ...positional };
		socket = new NetSocket();
		socket.connect({
			host: options.host ?? "127.0.0.1",
			port: options.port,
			localAddress: options.localAddress,
			localPort: options.localPort
		});
	}
  if (cb) socket.once("secureConnect", cb);
  const upgradeOptions = buildSerializedTlsOptions(
    options,
    {
      isServer: false,
      servername: options.servername ?? options.host ?? "127.0.0.1"
    }
  );
  socket.servername = upgradeOptions.servername;
  if (socket._connected) {
    socket._upgradeTls(upgradeOptions);
  } else {
    socket.once("connect", () => {
      socket._upgradeTls(upgradeOptions);
    });
  }
  return socket;
}

function matchesServername(pattern, servername) {
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

var TLSServer = class {
  _listeners = {};
  _onceListeners = {};
  _server;
  _tlsOptions;
  _sniCallback;
  _alpnCallback;
  _contexts = [];
  constructor(optionsOrListener, maybeListener) {
    const options = typeof optionsOrListener === "function" || optionsOrListener === void 0 ? void 0 : optionsOrListener;
    const listener = typeof optionsOrListener === "function" ? optionsOrListener : maybeListener;
    if (options?.ALPNCallback && options?.ALPNProtocols) {
      const error = new Error(
        "The ALPNCallback and ALPNProtocols TLS options are mutually exclusive"
      );
      error.code = "ERR_TLS_ALPN_CALLBACK_WITH_PROTOCOLS";
      throw error;
    }
    this._tlsOptions = buildSerializedTlsOptions(
      options,
      { isServer: true }
    );
    this._sniCallback = options?.SNICallback;
    this._alpnCallback = options?.ALPNCallback;
    this._server = new NetServer(
      options ? {
        allowHalfOpen: options.allowHalfOpen,
        keepAlive: options.keepAlive,
        keepAliveInitialDelay: options.keepAliveInitialDelay
      } : void 0,
      ((socket) => {
        const tlsSocket = socket;
        tlsSocket.server = this;
        void this._handleSecureSocket(tlsSocket);
      })
    );
    if (listener) {
      this.on("secureConnection", listener);
    }
    this._server.on("listening", (...args) => this._emit("listening", ...args));
    this._server.on("close", (...args) => this._emit("close", ...args));
    this._server.on("error", (...args) => this._emit("error", ...args));
    this._server.on("drop", (...args) => this._emit("drop", ...args));
  }
  listen(portOrOptions, hostOrCallback, backlogOrCallback, callback) {
    this._server.listen(portOrOptions, hostOrCallback, backlogOrCallback, callback);
    return this;
  }
  close(callback) {
    if (callback) {
      this.once("close", callback);
    }
    this._server.close();
    return this;
  }
  address() {
    return this._server.address();
  }
  getConnections(callback) {
    this._server.getConnections(callback);
    return this;
  }
  ref() {
    this._server.ref();
    return this;
  }
  unref() {
    this._server.unref();
    return this;
  }
  addContext(servername, context) {
    const wrapper = isTlsSecureContextWrapper(context) ? context : createSecureContextWrapper(
      context && typeof context === "object" ? context : void 0
    );
    this._contexts.push({ servername, context: wrapper });
    return this;
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    return this;
  }
  once(event, listener) {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    return this;
  }
  emit(event, ...args) {
    return this._emit(event, ...args);
  }
  _emit(event, ...args) {
    let handled = false;
    const listeners = this._listeners[event];
    if (listeners) {
      for (const fn of [...listeners]) {
        fn.call(this, ...args);
        handled = true;
      }
    }
    const onceListeners = this._onceListeners[event];
    if (onceListeners) {
      this._onceListeners[event] = [];
      for (const fn of [...onceListeners]) {
        fn.call(this, ...args);
        handled = true;
      }
    }
    return handled;
  }
  async _handleSecureSocket(socket) {
    const clientHello = this._getClientHello(socket);
    const requestedServername = clientHello?.servername;
    if (requestedServername) {
      socket.servername = requestedServername;
    }
    try {
      const upgradeOptions = await this._resolveTlsOptions(
        requestedServername,
        clientHello?.ALPNProtocols ?? []
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
      socket.on("error", (error) => {
        this._emit("tlsClientError", error, socket);
      });
    } catch (error) {
      const err = error instanceof Error ? error : new Error(String(error));
      this._emitTlsClientError(socket, err.message, err);
      if (err.uncaught) {
        process.emit?.("uncaughtException", err, "uncaughtException");
      }
    }
  }
  _getClientHello(socket) {
    if (typeof _netSocketGetTlsClientHelloRaw === "undefined") {
      return null;
    }
    const socketId = socket._socketId;
    if (typeof socketId !== "number" || socketId === 0) {
      return null;
    }
    return parseTlsClientHello(
      _netSocketGetTlsClientHelloRaw.applySync(void 0, [socketId])
    );
  }
  async _resolveTlsOptions(servername, clientProtocols) {
    let selectedContext = null;
    let invalidContext = false;
    if (servername && this._sniCallback) {
      selectedContext = await new Promise((resolve, reject) => {
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
          if (context && typeof context === "object" && Object.keys(context).length > 0) {
            resolve(createSecureContextWrapper(context));
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
    const resolvedOptions = {
      ...this._tlsOptions,
      ...selectedContext?.__agentOsTlsContext ?? {},
      isServer: true
    };
    if (this._alpnCallback) {
      const selectedProtocol = this._alpnCallback({
        servername,
        protocols: clientProtocols
      });
      if (selectedProtocol === void 0) {
        const error = new Error("ALPN callback rejected the client protocol list");
        error.code = "ERR_SSL_TLSV1_ALERT_NO_APPLICATION_PROTOCOL";
        throw error;
      }
      if (!clientProtocols.includes(selectedProtocol)) {
        const error = new Error(
          "The ALPNCallback callback returned an invalid protocol"
        );
        error.code = "ERR_TLS_ALPN_CALLBACK_INVALID_RESULT";
        error.uncaught = true;
        throw error;
      }
      resolvedOptions.ALPNProtocols = [selectedProtocol];
    }
    return resolvedOptions;
  }
  _findContext(servername) {
    for (let index = this._contexts.length - 1; index >= 0; index -= 1) {
      const entry = this._contexts[index];
      if (matchesServername(entry.servername, servername)) {
        return entry.context;
      }
    }
    return null;
  }
  _emitTlsClientError(socket, message, existingError) {
    const error = existingError ?? new Error(message);
    socket.servername ??= this._getClientHello(socket)?.servername;
    this._emit("tlsClientError", error, socket);
    socket.destroy();
  }
};

function TLSServerCallable(optionsOrListener, maybeListener) {
  return new TLSServer(optionsOrListener, maybeListener);
}

function checkServerIdentity(hostname, cert) {
  const host = String(hostname);
  const altNames = String(cert?.subjectaltname ?? "")
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
  const matchesAltName = altNames.some((entry) => {
    const separator = entry.indexOf(":");
    if (separator < 0) return false;
    const kind = entry.slice(0, separator).trim().toUpperCase();
    const value = entry.slice(separator + 1).trim();
    return kind === "DNS" ? matchesServername(value, host) : kind === "IP ADDRESS" && value === host;
  });
  const commonName = cert?.subject?.CN;
  if (matchesAltName || (altNames.length === 0 && typeof commonName === "string" && matchesServername(commonName, host))) {
    return undefined;
  }
  const error = new Error(`Hostname/IP does not match certificate's altnames: Host: ${host}`);
  error.code = "ERR_TLS_CERT_ALTNAME_INVALID";
  error.reason = "Host does not match certificate subject or alternative names";
  error.host = host;
  error.cert = cert;
  return error;
}

var tlsModule = {
  connect: tlsConnect,
  TLSSocket,
  Server: TLSServerCallable,
  createServer(optionsOrListener, maybeListener) {
    return new TLSServer(optionsOrListener, maybeListener);
  },
  createSecureContext(options) {
    return createSecureContextWrapper(options);
  },
  checkServerIdentity,
  getCiphers() {
    if (typeof _tlsGetCiphersRaw === "undefined") {
      throw new Error("tls.getCiphers is not supported in sandbox");
    }
    try {
      return JSON.parse(_tlsGetCiphersRaw.applySync(void 0, []));
    } catch {
      return [];
    }
  },
  DEFAULT_MIN_VERSION: "TLSv1.2",
  DEFAULT_MAX_VERSION: "TLSv1.3",
  rootCertificates: Object.freeze([])
};

exposeCustomGlobal("_netModule", netModule);
export { TLSServer, TLSServerCallable, TLSSocket, adoptRawTlsSocket, createSecureContextWrapper, matchesServername, tlsConnect, tlsModule };
