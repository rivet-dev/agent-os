import { once } from "./events.js";
import { import_buffer2 } from "./buffer-runtime.js";
import { _queueMicrotask } from "./timers.js";
import { _resolveRuntimeTtyConfig } from "./tty-config.js";

function _getStdoutIsTTY() {
  return _resolveRuntimeTtyConfig().stdoutIsTTY;
}

function _getStderrIsTTY() {
  return _resolveRuntimeTtyConfig().stderrIsTTY;
}

function getWriteCallback(encodingOrCallback, callback) {
  if (typeof encodingOrCallback === "function") {
    return encodingOrCallback;
  }
  if (typeof callback === "function") {
    return callback;
  }
  return void 0;
}

function emitListeners(listeners, onceListeners, event, args) {
  const persistent = listeners[event] ? listeners[event].slice() : [];
  const once = onceListeners[event] ? onceListeners[event].slice() : [];
  if (once.length > 0) {
    onceListeners[event] = [];
  }
  for (const listener of persistent) {
    listener(...args);
  }
  for (const listener of once) {
    listener(...args);
  }
  return persistent.length + once.length > 0;
}

function createStdioWriteStream(options) {
  const listeners = {};
  const onceListeners = {};
  let maxListeners = 10;
  const remove = (event, listener) => {
    if (listeners[event]) {
      const idx = listeners[event].indexOf(listener);
      if (idx !== -1) listeners[event].splice(idx, 1);
    }
    if (onceListeners[event]) {
      const idx = onceListeners[event].indexOf(listener);
      if (idx !== -1) onceListeners[event].splice(idx, 1);
    }
  };
  const stream = {
    write(data, encodingOrCallback, callback) {
      if (data instanceof Uint8Array || typeof import_buffer2.Buffer !== "undefined" && import_buffer2.Buffer.isBuffer(data)) {
        options.write(data);
      } else {
        options.write(String(data));
      }
      const done = getWriteCallback(encodingOrCallback, callback);
      if (done) {
        _queueMicrotask(() => done(null));
      }
      return true;
    },
    end(chunk, encoding, callback) {
      if (typeof chunk === "function") { callback = chunk; chunk = undefined; }
      else if (typeof encoding === "function") { callback = encoding; }
      if (chunk != null) stream.write(chunk);
      stream.writableEnded = true;
      if (typeof callback === "function") _queueMicrotask(() => callback());
      _queueMicrotask(() => emitListeners(listeners, onceListeners, "finish", []));
      return stream;
    },
    // Node Writable surface that process.stdout/stderr must expose (node-fidelity A7); these
    // streams are unbuffered host writes, so destroy/cork/uncork are no-ops that keep callers
    // (and the Claude EPIPE/buffered-destroy guards) on the standard path.
    destroyed: false,
    destroy(error) {
      if (stream.destroyed) return stream;
      stream.destroyed = true;
      if (error) _queueMicrotask(() => emitListeners(listeners, onceListeners, "error", [error]));
      _queueMicrotask(() => emitListeners(listeners, onceListeners, "close", []));
      return stream;
    },
    cork() {},
    uncork() {},
    setDefaultEncoding() { return stream; },
    on(event, listener) {
      if (!listeners[event]) listeners[event] = [];
      listeners[event].push(listener);
      return stream;
    },
    once(event, listener) {
      if (!onceListeners[event]) onceListeners[event] = [];
      onceListeners[event].push(listener);
      return stream;
    },
    off(event, listener) {
      remove(event, listener);
      return stream;
    },
    removeListener(event, listener) {
      remove(event, listener);
      return stream;
    },
    addListener(event, listener) {
      return stream.on(event, listener);
    },
    prependListener(event, listener) {
      if (!listeners[event]) listeners[event] = [];
      listeners[event].unshift(listener);
      return stream;
    },
    prependOnceListener(event, listener) {
      if (!onceListeners[event]) onceListeners[event] = [];
      onceListeners[event].unshift(listener);
      return stream;
    },
    removeAllListeners(event) {
      if (event === undefined) {
        for (const name of Object.keys(listeners)) delete listeners[name];
        for (const name of Object.keys(onceListeners)) delete onceListeners[name];
      } else {
        delete listeners[event];
        delete onceListeners[event];
      }
      return stream;
    },
    listeners(event) {
      return [...listeners[event] || [], ...onceListeners[event] || []];
    },
    listenerCount(event) {
      return (listeners[event]?.length || 0) + (onceListeners[event]?.length || 0);
    },
    setMaxListeners(value) {
      if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
        const error = new RangeError(`The value of "n" is out of range. It must be a non-negative number. Received ${value}.`);
        error.code = "ERR_OUT_OF_RANGE";
        throw error;
      }
      maxListeners = value;
      return stream;
    },
    getMaxListeners() {
      return maxListeners;
    },
    eventNames() {
      return [...new Set([...Object.keys(listeners), ...Object.keys(onceListeners)])];
    },
    emit(event, ...args) {
      return emitListeners(listeners, onceListeners, event, args);
    },
    writable: true,
    get isTTY() {
      return options.isTTY();
    },
    get columns() {
      return _resolveRuntimeTtyConfig().cols;
    },
    get rows() {
      return _resolveRuntimeTtyConfig().rows;
    },
    getColorDepth() {
      return options.isTTY() ? 8 : 1;
    },
    hasColors(count = 16) {
      if (!options.isTTY()) return false;
      const normalized = Number(count);
      return Number.isFinite(normalized) && normalized <= 2 ** 8;
    },
  };
  return stream;
}

var _stdout = createStdioWriteStream({
  write(data) {
    if (typeof _log !== "undefined") {
      _log.applySync(void 0, [data]);
    }
  },
  isTTY: _getStdoutIsTTY
});

var _stderr = createStdioWriteStream({
  write(data) {
    if (typeof _error !== "undefined") {
      _error.applySync(void 0, [data]);
    }
  },
  isTTY: _getStderrIsTTY
});

function formatConsoleValue(value) {
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "bigint") {
    return `${value}n`;
  }
  if (value instanceof Error) {
    return value.stack || value.message || String(value);
  }
  if (typeof value === "object" && value !== null) {
    try {
      return JSON.stringify(value);
    } catch {
    }
  }
  return String(value);
}

function formatConsoleArgs(args) {
  const builtinUtilModule = installBuiltinUtilFormatWithOptions(
    globalThis.__agentOsBuiltinUtilModule
  );
  if (typeof builtinUtilModule !== "undefined" && typeof builtinUtilModule?.formatWithOptions === "function") {
    return builtinUtilModule.formatWithOptions({ colors: false }, ...args);
  }
  return args.map((value) => formatConsoleValue(value)).join(" ");
}

function formatConsoleLine(args) {
  return `${formatConsoleArgs(args)}\n`;
}

class Console {
  constructor(stdout = _stdout, stderr = _stderr) {
    this._stdout = stdout;
    this._stderr = stderr;
    this._counts = new Map();
    this._times = new Map();
    for (const method of [
      "assert",
      "clear",
      "count",
      "countReset",
      "debug",
      "dir",
      "dirxml",
      "error",
      "group",
      "groupCollapsed",
      "groupEnd",
      "info",
      "log",
      "table",
      "time",
      "timeEnd",
      "timeLog",
      "trace",
      "warn"
    ]) {
      this[method] = this[method].bind(this);
    }
  }
  log(...args) {
    this._stdout.write(formatConsoleLine(args));
  }
  info(...args) {
    this._stdout.write(formatConsoleLine(args));
  }
  debug(...args) {
    this._stdout.write(formatConsoleLine(args));
  }
  warn(...args) {
    this._stderr.write(formatConsoleLine(args));
  }
  error(...args) {
    this._stderr.write(formatConsoleLine(args));
  }
  dir(value) {
    this._stdout.write(formatConsoleLine([value]));
  }
  dirxml(...args) {
    this.log(...args);
  }
  trace(...args) {
    const message = formatConsoleArgs(args);
    const error = new Error(message);
    this._stderr.write(`${error.stack || message}\n`);
  }
  assert(condition, ...args) {
    if (!condition) {
      const message = args.length > 0 ? formatConsoleArgs(args) : "Assertion failed";
      this._stderr.write(`${message}\n`);
    }
  }
  clear() {
  }
  count(label = "default") {
    const next = (this._counts.get(label) ?? 0) + 1;
    this._counts.set(label, next);
    this.log(`${label}: ${next}`);
  }
  countReset(label = "default") {
    this._counts.delete(label);
  }
  group(...args) {
    if (args.length > 0) {
      this.log(...args);
    }
  }
  groupCollapsed(...args) {
    if (args.length > 0) {
      this.log(...args);
    }
  }
  groupEnd() {
  }
  table(tabularData) {
    this.log(tabularData);
  }
  time(label = "default") {
    this._times.set(label, Date.now());
  }
  timeEnd(label = "default") {
    if (!this._times.has(label)) {
      return;
    }
    const startedAt = this._times.get(label);
    this._times.delete(label);
    this.log(`${label}: ${Date.now() - startedAt}ms`);
  }
  timeLog(label = "default", ...args) {
    if (!this._times.has(label)) {
      return;
    }
    const startedAt = this._times.get(label);
    this.log(`${label}: ${Date.now() - startedAt}ms`, ...args);
  }
}

const defaultConsole = new Console();

globalThis.console = defaultConsole;

function createConsoleTask() {
  return {
    run(callback, ...args) {
      return typeof callback === "function" ? callback(...args) : void 0;
    }
  };
}

function consoleContext(stdout = _stdout, stderr = _stderr) {
  return new Console(stdout, stderr);
}

var builtinConsoleModule = {
  Console,
  assert: defaultConsole.assert.bind(defaultConsole),
  clear: defaultConsole.clear.bind(defaultConsole),
  context: consoleContext,
  count: defaultConsole.count.bind(defaultConsole),
  countReset: defaultConsole.countReset.bind(defaultConsole),
  createTask: createConsoleTask,
  debug: defaultConsole.debug.bind(defaultConsole),
  dir: defaultConsole.dir.bind(defaultConsole),
  dirxml: defaultConsole.dirxml.bind(defaultConsole),
  error: defaultConsole.error.bind(defaultConsole),
  group: defaultConsole.group.bind(defaultConsole),
  groupCollapsed: defaultConsole.groupCollapsed.bind(defaultConsole),
  groupEnd: defaultConsole.groupEnd.bind(defaultConsole),
  info: defaultConsole.info.bind(defaultConsole),
  log: defaultConsole.log.bind(defaultConsole),
  profile: void 0,
  profileEnd: void 0,
  table: defaultConsole.table.bind(defaultConsole),
  time: defaultConsole.time.bind(defaultConsole),
  timeEnd: defaultConsole.timeEnd.bind(defaultConsole),
  timeLog: defaultConsole.timeLog.bind(defaultConsole),
  timeStamp: void 0,
  trace: defaultConsole.trace.bind(defaultConsole),
  warn: defaultConsole.warn.bind(defaultConsole)
};

function installBuiltinUtilFormatWithOptions(builtinUtilModule) {
  if (!builtinUtilModule) {
    return builtinUtilModule;
  }
  if (typeof builtinUtilModule.aborted !== "function") {
    builtinUtilModule.aborted = function aborted(signal) {
      if (!signal || typeof signal.addEventListener !== "function") {
        const error = new TypeError('The "signal" argument must be an AbortSignal');
        error.code = "ERR_INVALID_ARG_TYPE";
        throw error;
      }
      if (signal.aborted) return Promise.resolve();
      return new Promise((resolve) => {
        signal.addEventListener("abort", resolve, { once: true });
      });
    };
  }
  const customPromisifySymbol = Symbol.for("nodejs.util.promisify.custom");
  if (
    typeof builtinUtilModule.promisify === "function" &&
    builtinUtilModule.promisify.custom !== customPromisifySymbol
  ) {
    const fallbackPromisify = builtinUtilModule.promisify;
    const nodeCompatiblePromisify = function promisify(original) {
      const custom = original?.[customPromisifySymbol];
      if (custom !== undefined) {
        if (typeof custom !== "function") {
          throw new TypeError('The "util.promisify.custom" property must be of type Function');
        }
        return custom;
      }
      return fallbackPromisify(original);
    };
    nodeCompatiblePromisify.custom = customPromisifySymbol;
    builtinUtilModule.promisify = nodeCompatiblePromisify;
  }
  if (typeof builtinUtilModule.styleText !== "function") {
    const styleCodes: Record<string, readonly [number, number]> = {
      reset: [0, 0], bold: [1, 22], dim: [2, 22], italic: [3, 23], underline: [4, 24],
      blink: [5, 25], inverse: [7, 27], hidden: [8, 28], strikethrough: [9, 29],
      black: [30, 39], red: [31, 39], green: [32, 39], yellow: [33, 39], blue: [34, 39],
      magenta: [35, 39], cyan: [36, 39], white: [37, 39], gray: [90, 39], grey: [90, 39],
      bgBlack: [40, 49], bgRed: [41, 49], bgGreen: [42, 49], bgYellow: [43, 49],
      bgBlue: [44, 49], bgMagenta: [45, 49], bgCyan: [46, 49], bgWhite: [47, 49],
    };
    builtinUtilModule.styleText = function styleText(format, text) {
      const value = String(text);
      const formats = Array.isArray(format) ? format : [format];
      for (const name of formats) {
        if (!Object.prototype.hasOwnProperty.call(styleCodes, name)) {
          throw new TypeError(`Unknown style format: ${name}`);
        }
      }
      if (process?.env?.NO_COLOR || process?.env?.FORCE_COLOR === "0") {
        return value;
      }
      return formats.reduceRight((result, name) => {
        const [open, close] = styleCodes[name];
        return `\u001b[${open}m${result}\u001b[${close}m`;
      }, value);
    };
  }
  if (typeof builtinUtilModule.stripVTControlCharacters !== "function") {
    const ansiEscapePattern = /(?:\u001B\][\s\S]*?(?:\u0007|\u001B\u005C|\u009C))|(?:[\u001B\u009B][[\]()#;?]*(?:(?:(?:[a-zA-Z\d]*(?:;[-a-zA-Z\d/#&.:=?%@~_]+)*)?\u0007)|(?:(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~])))/g;
    builtinUtilModule.stripVTControlCharacters = function stripVTControlCharacters(value) {
      if (typeof value !== "string") {
        const error = new TypeError(`The "str" argument must be of type string. Received ${value === null ? "null" : typeof value}`);
        error.code = "ERR_INVALID_ARG_TYPE";
        throw error;
      }
      return value.replace(ansiEscapePattern, "");
    };
  }
  if (typeof builtinUtilModule.parseEnv !== "function") {
    const envLinePattern = /(?:^|\n)\s*(?:export\s+)?([\w.-]+)\s*=\s*(?:'((?:\\'|[^'])*)'|"((?:\\"|[^\"])*)"|`((?:\\`|[^`])*)`|([^#\r\n]*?))\s*(?:#[^\r\n]*)?(?=\r?\n|$)/g;
    builtinUtilModule.parseEnv = function parseEnv(content) {
      if (typeof content !== "string") {
        const received = content === null ? "null" : `type ${typeof content} (${String(content)})`;
        const error = new TypeError(`The "content" argument must be of type string. Received ${received}`);
        error.code = "ERR_INVALID_ARG_TYPE";
        throw error;
      }
      const result = {};
      envLinePattern.lastIndex = 0;
      for (let match; (match = envLinePattern.exec(content)) !== null;) {
        let value;
        if (match[2] !== undefined) {
          value = match[2];
        } else if (match[3] !== undefined) {
          value = match[3]
            .replace(/\\n/g, "\n")
            .replace(/\\r/g, "\r")
            .replace(/\\t/g, "\t")
            .replace(/\\"/g, '"');
        } else if (match[4] !== undefined) {
          value = match[4];
        } else {
          value = (match[5] ?? "").trim();
        }
        result[match[1]] = value;
      }
      return result;
    };
  }
  if (typeof builtinUtilModule.formatWithOptions === "function") {
    return builtinUtilModule;
  }
  builtinUtilModule.formatWithOptions = function formatWithOptions(inspectOptions, format, ...args) {
    const inspectValue = (value) => {
      if (typeof builtinUtilModule.inspect === "function") {
        return builtinUtilModule.inspect(value, inspectOptions);
      }
      try {
        return JSON.stringify(value);
      } catch {
        return String(value);
      }
    };
    const formatValue = (value) => typeof value === "string" ? value : inspectValue(value);
    if (typeof format !== "string") {
      return [format, ...args].map(formatValue).join(" ");
    }
    let index = 0;
    const formatted = format.replace(/%[sdifjoO%]/g, (token) => {
      if (token === "%%") {
        return "%";
      }
      if (index >= args.length) {
        return token;
      }
      const value = args[index++];
      switch (token) {
        case "%s":
          return String(value);
        case "%d":
          return Number(value).toString();
        case "%i":
          return Number.parseInt(value, 10).toString();
        case "%f":
          return Number.parseFloat(value).toString();
        case "%j":
          try {
            return JSON.stringify(value);
          } catch {
            return "[Circular]";
          }
        case "%o":
        case "%O":
          return inspectValue(value);
        default:
          return token;
      }
    });
    if (index >= args.length) {
      return formatted;
    }
    return [formatted, ...args.slice(index).map(formatValue)].join(" ");
  };
  return builtinUtilModule;
}
export { Console, _getStderrIsTTY, _getStdoutIsTTY, _stderr, _stdout, builtinConsoleModule, consoleContext, createConsoleTask, createStdioWriteStream, defaultConsole, emitListeners, formatConsoleArgs, formatConsoleLine, formatConsoleValue, getWriteCallback, installBuiltinUtilFormatWithOptions };
