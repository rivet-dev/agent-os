import { EventEmitter, once } from "./events.js";
import { process2 } from "./process.js";

function createWorkerThreadsNotImplementedError(feature) {
  const error = new Error(`node:worker_threads ${feature} is not available in the agentos guest runtime`);
  error.code = "ERR_NOT_IMPLEMENTED";
  return error;
}

class WorkerThreadPort extends EventEmitter {
  postMessage() {
  }
  start() {
  }
  close() {
    this.emit("close");
  }
  unref() {
    return this;
  }
  ref() {
    return this;
  }
}

class WorkerThreadMessageChannel {
  constructor() {
    this.port1 = new WorkerThreadPort();
    this.port2 = new WorkerThreadPort();
  }
}

class WorkerThreadWorker extends EventEmitter {
  constructor() {
    super();
    throw createWorkerThreadsNotImplementedError("Worker");
  }
}

var builtinWorkerThreadsModule = {
  BroadcastChannel: globalThis.BroadcastChannel,
  MessageChannel: globalThis.MessageChannel ?? WorkerThreadMessageChannel,
  MessagePort: globalThis.MessagePort ?? WorkerThreadPort,
  SHARE_ENV: Symbol.for("agentos.worker_threads.SHARE_ENV"),
  Worker: WorkerThreadWorker,
  getEnvironmentData() {
    return void 0;
  },
  isMainThread: true,
  markAsUncloneable() {
  },
  markAsUntransferable() {
  },
  moveMessagePortToContext() {
    throw createWorkerThreadsNotImplementedError("moveMessagePortToContext");
  },
  parentPort: null,
  postMessageToThread() {
    throw createWorkerThreadsNotImplementedError("postMessageToThread");
  },
  receiveMessageOnPort() {
    return void 0;
  },
  resourceLimits: {},
  setEnvironmentData() {
  },
  threadId: 0,
  workerData: null
};

// NOTE: read the live `process2` import lazily inside each function (call time,
// after all modules have initialized). A load-time snapshot `var process_default =
// process2` would capture `undefined`, because misc-stubs evaluates far earlier than
// process.ts in the bundle's module-cycle order.
function ttyIsatty(fd) {
  if (fd === 0) {
    return !!process2.stdin?.isTTY;
  }
  if (fd === 1) {
    return !!process2.stdout?.isTTY;
  }
  if (fd === 2) {
    return !!process2.stderr?.isTTY;
  }
  return false;
}

function TtyReadStream(fd) {
  return fd === 0 ? process2.stdin : void 0;
}

function TtyWriteStream(fd) {
  if (fd === 1) {
    return process2.stdout;
  }
  if (fd === 2) {
    return process2.stderr;
  }
  return void 0;
}

var builtinTtyModule = {
  ReadStream: class ReadStream {
    constructor(fd) {
      return TtyReadStream(fd);
    }
  },
  WriteStream: class WriteStream {
    constructor(fd) {
      return TtyWriteStream(fd);
    }
    getColorDepth() {
      return this?.isTTY ? 8 : 1;
    }
    hasColors(count = 16) {
      return !!this?.isTTY && Number(count) <= 2 ** 8;
    }
  },
  isatty: ttyIsatty
};

async function collectReadableChunks(input) {
  const readable = getNodeReadableAsyncIterable(input);
  if (readable) {
    const chunks = [];
    for await (const chunk of readable) {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk ?? []));
    }
    return chunks;
  }
  if (input && typeof input[Symbol.asyncIterator] === "function") {
    const chunks = [];
    for await (const chunk of input) {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk ?? []));
    }
    return chunks;
  }
  if (input && typeof input.getReader === "function") {
    const reader = input.getReader();
    const chunks = [];
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        chunks.push(Buffer.from(value ?? []));
      }
    } finally {
      reader.releaseLock?.();
    }
    return chunks;
  }
  throw new TypeError("expected an async iterable or WHATWG ReadableStream");
}

function createBuiltinBlob(buffer, type = "") {
  return {
    size: buffer.byteLength,
    type,
    async arrayBuffer() {
      return buffer.buffer.slice(buffer.byteOffset, buffer.byteOffset + buffer.byteLength);
    },
    stream() {
      return new ReadableStream({
        start(controller) {
          controller.enqueue(buffer);
          controller.close();
        }
      });
    },
    async text() {
      return buffer.toString("utf8");
    }
  };
}

var builtinStreamConsumersModule = {
  async arrayBuffer(stream) {
    const chunks = await collectReadableChunks(stream);
    const buffer = Buffer.concat(chunks);
    return buffer.buffer.slice(buffer.byteOffset, buffer.byteOffset + buffer.byteLength);
  },
  async blob(stream) {
    return createBuiltinBlob(await builtinStreamConsumersModule.buffer(stream));
  },
  async buffer(stream) {
    return Buffer.concat(await collectReadableChunks(stream));
  },
  async json(stream) {
    return JSON.parse(await builtinStreamConsumersModule.text(stream));
  },
  async text(stream) {
    return (await builtinStreamConsumersModule.buffer(stream)).toString("utf8");
  }
};

function getNodeReadableAsyncIterable(stream) {
  if (
    !stream ||
    typeof stream.on !== "function" ||
    (typeof stream.read !== "function" &&
      typeof stream.pipe !== "function" &&
      typeof stream.resume !== "function")
  ) {
    return null;
  }
  return {
    async *[Symbol.asyncIterator]() {
      const queuedChunks = [];
      const pendingResolves = [];
      let done = false;
      let error = null;
      const cleanup = [];
      const removeListener =
        typeof stream.off === "function"
          ? stream.off.bind(stream)
          : typeof stream.removeListener === "function"
            ? stream.removeListener.bind(stream)
            : null;
      const flush = () => {
        while (pendingResolves.length > 0) {
          if (error) {
            pendingResolves.shift()?.(Promise.reject(error));
            continue;
          }
          if (queuedChunks.length > 0) {
            pendingResolves.shift()?.(
              Promise.resolve({ done: false, value: queuedChunks.shift() })
            );
            continue;
          }
          if (done) {
            pendingResolves.shift()?.(Promise.resolve({ done: true, value: void 0 }));
            continue;
          }
          break;
        }
      };
      const add = (eventName, handler) => {
        stream.on(eventName, handler);
        cleanup.push(() => removeListener?.(eventName, handler));
      };
      const onData = (chunk) => {
        queuedChunks.push(chunk);
        flush();
      };
      const onEnd = () => {
        done = true;
        flush();
      };
      const onError = (reason) => {
        error = reason;
        done = true;
        flush();
      };
      add("data", onData);
      add("end", onEnd);
      add("close", onEnd);
      add("error", onError);
      stream.resume?.();
      try {
        while (true) {
          if (error) {
            throw error;
          }
          if (queuedChunks.length > 0) {
            yield queuedChunks.shift();
            continue;
          }
          if (done) {
            return;
          }
          const result = await new Promise((resolve) => {
            pendingResolves.push(resolve);
          });
          if (result.done) {
            return;
          }
          yield result.value;
        }
      } finally {
        while (cleanup.length > 0) {
          cleanup.pop()?.();
        }
      }
    }
  };
}

var builtinStreamPromisesModule = {
  finished(stream) {
    return new Promise((resolve, reject) => {
      if (!stream || typeof stream !== "object") {
        reject(new TypeError("finished() expects a stream"));
        return;
      }
      const cleanup = [];
      const add = (eventName, handler) => {
        stream?.once?.(eventName, handler);
        cleanup.push(() => stream?.off?.(eventName, handler));
      };
      const settle = (callback) => (value) => {
        while (cleanup.length > 0) {
          cleanup.pop()?.();
        }
        callback(value);
      };
      add("finish", settle(resolve));
      add("end", settle(resolve));
      add("close", settle(resolve));
      add("error", settle(reject));
    });
  },
  async pipeline(source, destination) {
    const readable =
      getNodeReadableAsyncIterable(source) ??
      (source && typeof source[Symbol.asyncIterator] === "function"
        ? source
        : source && typeof source.getReader === "function"
          ? {
              async *[Symbol.asyncIterator]() {
                const reader = source.getReader();
                try {
                  while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    yield Buffer.from(value ?? []);
                  }
                } finally {
                  reader.releaseLock?.();
                }
              }
            }
          : null);
    if (readable == null) {
      throw new TypeError("pipeline source must be async iterable or a WHATWG ReadableStream");
    }
    if (!destination || typeof destination.write !== "function") {
      throw new TypeError("pipeline destination must provide write()");
    }
    for await (const chunk of readable) {
      await new Promise((resolve, reject) => {
        try {
          destination.write(chunk, (error) => error ? reject(error) : resolve());
        } catch (error) {
          reject(error);
        }
      });
    }
    const completion = builtinStreamPromisesModule.finished(destination);
    if (typeof destination.end === "function") {
      await new Promise((resolve, reject) => {
        try {
          destination.end((error) => error ? reject(error) : resolve());
        } catch (error) {
          reject(error);
        }
      });
    }
    await completion;
    return destination;
  }
};

function createAccessDeniedBuiltinError(request) {
  const normalized = String(request).replace(/^node:/, "");
  const error = new Error(`node:${normalized} is not available in the agentos guest runtime`);
  error.code = "ERR_ACCESS_DENIED";
  return error;
}

class DiagnosticsChannel {
  constructor(name = "") {
    this.name = String(name);
    this._subscribers = /* @__PURE__ */ new Set();
  }
  get hasSubscribers() {
    return this._subscribers.size > 0;
  }
  publish(message) {
    for (const subscriber of Array.from(this._subscribers)) {
      subscriber(message, this.name);
    }
  }
  subscribe(subscriber) {
    if (typeof subscriber === "function") {
      this._subscribers.add(subscriber);
    }
  }
  unsubscribe(subscriber) {
    return this._subscribers.delete(subscriber);
  }
  runStores(context, callback, thisArg, ...args) {
    if (typeof callback !== "function") {
      return callback;
    }
    return callback.apply(thisArg, args);
  }
}

var diagnosticsChannelCache = /* @__PURE__ */ new Map();

function getDiagnosticsChannel(name = "") {
  const channelName = String(name);
  let existing = diagnosticsChannelCache.get(channelName);
  if (!existing) {
    existing = new DiagnosticsChannel(channelName);
    diagnosticsChannelCache.set(channelName, existing);
  }
  return existing;
}

function createDiagnosticsTracingChannel(name = "") {
  const channelName = String(name);
  const tracing = {
    start: getDiagnosticsChannel(`tracing:${channelName}:start`),
    end: getDiagnosticsChannel(`tracing:${channelName}:end`),
    asyncStart: getDiagnosticsChannel(`tracing:${channelName}:asyncStart`),
    asyncEnd: getDiagnosticsChannel(`tracing:${channelName}:asyncEnd`),
    error: getDiagnosticsChannel(`tracing:${channelName}:error`),
    subscribe() {
    },
    unsubscribe() {
      return true;
    },
    traceSync(fn, context, thisArg, ...args) {
      if (typeof fn !== "function") {
        return fn;
      }
      return fn.apply(thisArg, args);
    },
    tracePromise(fn, context, thisArg, ...args) {
      if (typeof fn !== "function") {
        return Promise.resolve(fn);
      }
      return Promise.resolve(fn.apply(thisArg, args));
    },
    traceCallback(fn, position, context, thisArg, ...args) {
      if (typeof fn !== "function") {
        return fn;
      }
      return fn.apply(thisArg, args);
    }
  };
  Object.defineProperty(tracing, "hasSubscribers", {
    get() {
      return tracing.start.hasSubscribers || tracing.end.hasSubscribers || tracing.asyncStart.hasSubscribers || tracing.asyncEnd.hasSubscribers || tracing.error.hasSubscribers;
    },
    enumerable: false,
    configurable: true
  });
  return tracing;
}

var builtinDiagnosticsChannelModule = {
  Channel: DiagnosticsChannel,
  channel: getDiagnosticsChannel,
  hasSubscribers(name = "") {
    return getDiagnosticsChannel(name).hasSubscribers;
  },
  subscribe(name = "", subscriber) {
    return getDiagnosticsChannel(name).subscribe(subscriber);
  },
  tracingChannel: createDiagnosticsTracingChannel,
  unsubscribe(name = "", subscriber) {
    return getDiagnosticsChannel(name).unsubscribe(subscriber);
  }
};

class InspectorSession extends EventEmitter {
  connect() {
  }
  connectToMainThread() {
  }
  disconnect() {
  }
  post(method, params, callback) {
    const done = typeof params === "function" ? params : callback;
    if (typeof done === "function") {
      queueMicrotask(() => done(null, {}));
    }
  }
}

var builtinInspectorModule = {
  Session: InspectorSession,
  close() {
  },
  console,
  open() {
  },
  url() {
    return void 0;
  },
  waitForDebugger() {
  },
};

function padDateTimeField(value, length = 2) {
  return String(Math.trunc(value)).padStart(length, "0");
}

function coerceIntlDate(value) {
  const date = value instanceof Date ? value : new Date(value ?? Date.now());
  if (Number.isNaN(date.getTime())) {
    throw new RangeError("Invalid time value");
  }
  return date;
}

function formatSafeDateTimeValue(value, options = {}) {
  const date = coerceIntlDate(value);
  const normalizedOptions = options && typeof options === "object" ? options : {};
  const year = padDateTimeField(date.getUTCFullYear(), 4);
  const month = padDateTimeField(date.getUTCMonth() + 1);
  const day = padDateTimeField(date.getUTCDate());
  const hour = padDateTimeField(date.getUTCHours());
  const minute = padDateTimeField(date.getUTCMinutes());
  const second = padDateTimeField(date.getUTCSeconds());
  const datePart = `${year}-${month}-${day}`;
  const timePart = `${hour}:${minute}:${second}`;
  const wantsDate = normalizedOptions.dateStyle || normalizedOptions.year || normalizedOptions.month || normalizedOptions.day || !normalizedOptions.timeStyle && !normalizedOptions.hour && !normalizedOptions.minute && !normalizedOptions.second;
  const wantsTime = normalizedOptions.timeStyle || normalizedOptions.hour || normalizedOptions.minute || normalizedOptions.second;
  if (wantsDate && wantsTime) {
    return `${datePart}, ${timePart}`;
  }
  if (wantsTime) {
    return timePart;
  }
  return datePart;
}

class SafeDateTimeFormatInstance {
  constructor(locales = "en-US", options = {}) {
    this.locales = locales;
    this.options = options && typeof options === "object" ? { ...options } : {};
    this.format = this.format.bind(this);
  }
  format(value = Date.now()) {
    return formatSafeDateTimeValue(value, this.options);
  }
  formatToParts(value = Date.now()) {
    return [{ type: "literal", value: this.format(value) }];
  }
  formatRange(start, end) {
    return `${this.format(start)} – ${this.format(end)}`;
  }
  formatRangeToParts(start, end) {
    return [{ type: "literal", value: this.formatRange(start, end), source: "shared" }];
  }
  resolvedOptions() {
    const locale = Array.isArray(this.locales) ? this.locales.find((entry) => typeof entry === "string") || "en-US" : typeof this.locales === "string" ? this.locales : "en-US";
    return {
      locale,
      calendar: "gregory",
      numberingSystem: "latn",
      timeZone: "UTC",
      ...this.options
    };
  }
  static supportedLocalesOf(locales) {
    if (Array.isArray(locales)) {
      return locales.filter((entry) => typeof entry === "string");
    }
    return typeof locales === "string" ? [locales] : [];
  }
}

// ECMA-402 constructors are deliberately callable with or without `new`.
// Keep the implementation in a class, but expose a normal function whose
// explicit return value preserves both call forms and `instanceof` behavior.
function SafeDateTimeFormat(locales = "en-US", options = {}) {
  return new SafeDateTimeFormatInstance(locales, options);
}
SafeDateTimeFormat.prototype = SafeDateTimeFormatInstance.prototype;
Object.defineProperty(SafeDateTimeFormat.prototype, "constructor", {
  value: SafeDateTimeFormat,
  configurable: true,
  writable: true
});
SafeDateTimeFormat.supportedLocalesOf = SafeDateTimeFormatInstance.supportedLocalesOf;

function normalizeFractionDigitOption(value, fallback) {
  const number = Number(value);
  if (!Number.isFinite(number)) return fallback;
  return Math.min(20, Math.max(0, Math.trunc(number)));
}

function applySafeNumberGrouping(value) {
  const [integer, fraction] = value.split(".");
  const sign = integer.startsWith("-") ? "-" : "";
  const digits = sign ? integer.slice(1) : integer;
  const grouped = digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return fraction === void 0 ? `${sign}${grouped}` : `${sign}${grouped}.${fraction}`;
}

class SafeNumberFormatInstance {
  constructor(locales = "en-US", options = {}) {
    this.locales = locales;
    this.options = options && typeof options === "object" ? { ...options } : {};
    this.format = this.format.bind(this);
  }
  format(value) {
    const number = Number(value);
    if (Number.isNaN(number)) return "NaN";
    if (number === Infinity) return "∞";
    if (number === -Infinity) return "-∞";
    const minimumFractionDigits = normalizeFractionDigitOption(this.options.minimumFractionDigits, 0);
    const maximumFractionDigits = Math.max(
      minimumFractionDigits,
      normalizeFractionDigitOption(this.options.maximumFractionDigits, Math.max(minimumFractionDigits, 3))
    );
    let formatted = number.toFixed(maximumFractionDigits);
    if (maximumFractionDigits > minimumFractionDigits) {
      formatted = formatted.replace(/(\.\d*?)0+$/, "$1").replace(/\.$/, "");
      const fractionLength = formatted.includes(".") ? formatted.length - formatted.indexOf(".") - 1 : 0;
      if (fractionLength < minimumFractionDigits) {
        formatted += `${fractionLength === 0 ? "." : ""}${"0".repeat(minimumFractionDigits - fractionLength)}`;
      }
    }
    if (this.options.useGrouping === false) return formatted;
    return applySafeNumberGrouping(formatted);
  }
  formatToParts(value) {
    return [{ type: "literal", value: this.format(value) }];
  }
  resolvedOptions() {
    const locale = Array.isArray(this.locales) ? this.locales.find((entry) => typeof entry === "string") || "en-US" : typeof this.locales === "string" ? this.locales : "en-US";
    return {
      locale,
      numberingSystem: "latn",
      style: "decimal",
      minimumFractionDigits: normalizeFractionDigitOption(this.options.minimumFractionDigits, 0),
      maximumFractionDigits: normalizeFractionDigitOption(this.options.maximumFractionDigits, 3),
      useGrouping: this.options.useGrouping !== false,
      ...this.options
    };
  }
  static supportedLocalesOf(locales) {
    if (Array.isArray(locales)) {
      return locales.filter((entry) => typeof entry === "string");
    }
    return typeof locales === "string" ? [locales] : [];
  }
}

class SafeListFormat {
  constructor(locales = "en-US", options = {}) {
    this.locales = locales;
    this.options = options && typeof options === "object" ? { ...options } : {};
    this.format = this.format.bind(this);
  }
  format(values) {
    const items = Array.from(values ?? [], (value) => String(value));
    if (items.length < 2) return items[0] ?? "";
    const conjunction = this.options.type === "disjunction" ? "or" : "and";
    if (items.length === 2) return `${items[0]} ${conjunction} ${items[1]}`;
    return `${items.slice(0, -1).join(", ")}, ${conjunction} ${items.at(-1)}`;
  }
  formatToParts(values) {
    return [{ type: "element", value: this.format(values) }];
  }
  resolvedOptions() {
    const locale = Array.isArray(this.locales) ? this.locales.find((entry) => typeof entry === "string") || "en-US" : typeof this.locales === "string" ? this.locales : "en-US";
    return {
      locale,
      style: this.options.style ?? "long",
      type: this.options.type ?? "conjunction"
    };
  }
  static supportedLocalesOf(locales) {
    if (Array.isArray(locales)) return locales.filter((entry) => typeof entry === "string");
    return typeof locales === "string" ? [locales] : [];
  }
}

function SafeNumberFormat(locales = "en-US", options = {}) {
  return new SafeNumberFormatInstance(locales, options);
}
SafeNumberFormat.prototype = SafeNumberFormatInstance.prototype;
Object.defineProperty(SafeNumberFormat.prototype, "constructor", {
  value: SafeNumberFormat,
  configurable: true,
  writable: true
});
SafeNumberFormat.supportedLocalesOf = SafeNumberFormatInstance.supportedLocalesOf;

function installSafeIntlFormatters(target) {
  const existingIntl = target.Intl && typeof target.Intl === "object" ? target.Intl : {};
  existingIntl.DateTimeFormat = SafeDateTimeFormat;
  existingIntl.NumberFormat = SafeNumberFormat;
  existingIntl.ListFormat = SafeListFormat;
  target.Intl = existingIntl;
  Date.prototype.toLocaleString = function(locales, options) {
    return new target.Intl.DateTimeFormat(locales, options).format(this);
  };
  Date.prototype.toLocaleDateString = function(locales, options) {
    return new target.Intl.DateTimeFormat(locales, { ...(options || {}), hour: void 0, minute: void 0, second: void 0 }).format(this);
  };
  Date.prototype.toLocaleTimeString = function(locales, options) {
    return new target.Intl.DateTimeFormat(locales, {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      ...(options || {})
    }).format(this);
  };
  Number.prototype.toLocaleString = function(locales, options) {
    return new target.Intl.NumberFormat(locales, options).format(this.valueOf());
  };
}
export { DiagnosticsChannel, InspectorSession, SafeDateTimeFormat, SafeListFormat, SafeNumberFormat, TtyReadStream, TtyWriteStream, WorkerThreadMessageChannel, WorkerThreadPort, WorkerThreadWorker, applySafeNumberGrouping, builtinDiagnosticsChannelModule, builtinInspectorModule, builtinStreamConsumersModule, builtinStreamPromisesModule, builtinTtyModule, builtinWorkerThreadsModule, coerceIntlDate, collectReadableChunks, createAccessDeniedBuiltinError, createBuiltinBlob, createDiagnosticsTracingChannel, createWorkerThreadsNotImplementedError, diagnosticsChannelCache, formatSafeDateTimeValue, getDiagnosticsChannel, getNodeReadableAsyncIterable, installSafeIntlFormatters, normalizeFractionDigitOption, padDateTimeField, ttyIsatty };
