import { once } from "./events.js";
import { exposeCustomGlobal, exposeMutableRuntimeStateGlobal } from "../global-exposure.js";
import { TextDecoder } from "../polyfills/index.js";
import { import_buffer2 } from "./buffer-runtime.js";
import { _getStdinIsTTY, isProcessExitError, routeAsyncCallbackError, scheduleAsyncRethrow } from "./process.js";

var _stdinListeners = {};

var _stdinOnceListeners = {};

var _stdinLiveDecoder = new TextDecoder();

var STDIN_HANDLE_ID = "process.stdin";

var _stdinLiveBuffer = "";

var _stdinLiveStarted = false;

var _stdinLiveHandleRegistered = false;

var _stdinLiveTerminalEventsScheduled = false;

var _stdinLiveTerminalEventsEmitted = false;

exposeMutableRuntimeStateGlobal(
  "_stdinData",
  typeof _processConfig !== "undefined" && _processConfig.stdin || ""
);

exposeMutableRuntimeStateGlobal("_stdinPosition", 0);

exposeMutableRuntimeStateGlobal("_stdinEnded", false);

exposeMutableRuntimeStateGlobal("_stdinFlowMode", false);

function getStdinData() {
  return globalThis._stdinData;
}

function setStdinDataValue(v) {
  globalThis._stdinData = v;
}

function getStdinPosition() {
  return globalThis._stdinPosition;
}

function setStdinPosition(v) {
  globalThis._stdinPosition = v;
}

function getStdinEnded() {
  return globalThis._stdinEnded;
}

function setStdinEnded(v) {
  globalThis._stdinEnded = v;
}

function getStdinFlowMode() {
  return globalThis._stdinFlowMode;
}

function setStdinFlowMode(v) {
  globalThis._stdinFlowMode = v;
}

function resetLiveStdinState(decoder) {
  _stdinLiveBuffer = "";
  _stdinLiveStarted = false;
  _stdinLiveDecoder = decoder;
  _stdinLiveTerminalEventsScheduled = false;
  _stdinLiveTerminalEventsEmitted = false;
}

function _emitStdinData() {
  if (getStdinEnded() || !getStdinData()) return;
  if (getStdinFlowMode() && getStdinPosition() < getStdinData().length) {
    const chunk = getStdinData().slice(getStdinPosition());
    setStdinPosition(getStdinData().length);
    const dataListeners = [..._stdinListeners["data"] || [], ..._stdinOnceListeners["data"] || []];
    _stdinOnceListeners["data"] = [];
    for (const listener of dataListeners) {
      listener(chunk);
    }
    setStdinEnded(true);
    const endListeners = [..._stdinListeners["end"] || [], ..._stdinOnceListeners["end"] || []];
    _stdinOnceListeners["end"] = [];
    for (const listener of endListeners) {
      listener();
    }
    const closeListeners = [..._stdinListeners["close"] || [], ..._stdinOnceListeners["close"] || []];
    _stdinOnceListeners["close"] = [];
    for (const listener of closeListeners) {
      listener();
    }
  }
}

function emitStdinListeners(event, value) {
  const listeners = [..._stdinListeners[event] || [], ..._stdinOnceListeners[event] || []];
  _stdinOnceListeners[event] = [];
  for (const listener of listeners) {
    try {
      listener(value);
    } catch (error) {
      const outcome = routeAsyncCallbackError(error);
      if (!outcome.handled && outcome.rethrow !== null) {
        if (isProcessExitError(outcome.rethrow)) {
          scheduleAsyncRethrow(outcome.rethrow);
          return true;
        }
        throw outcome.rethrow;
      }
      return true;
    }
  }
  return listeners.length > 0;
}

function syncLiveStdinHandle(active) {
  if (active) {
    if (!_stdinLiveHandleRegistered && typeof _registerHandle === "function") {
      try {
        _registerHandle(STDIN_HANDLE_ID, "process.stdin");
        _stdinLiveHandleRegistered = true;
      } catch {
      }
    }
    return;
  }
  if (_stdinLiveHandleRegistered && typeof _unregisterHandle === "function") {
    try {
      _unregisterHandle(STDIN_HANDLE_ID);
    } catch {
    }
    _stdinLiveHandleRegistered = false;
  }
}

function configureLiveStdin(active, eager = false) {
  globalThis.__runtimeStreamStdin = !!active;
  syncLiveStdinHandle(!!active && !!eager && !getStdinEnded());
}

exposeCustomGlobal("__runtimeConfigureStreamStdin", configureLiveStdin);

function flushLiveStdinBuffer() {
  if (!getStdinFlowMode() || _stdinLiveBuffer.length === 0) return;
  const chunk = _stdinLiveBuffer;
  _stdinLiveBuffer = "";
  const data = _stdin.encoding ? chunk : import_buffer2.Buffer.from(chunk);
  emitStdinListeners("data", data);
  maybeEmitLiveStdinTerminalEvents();
}

function maybeEmitLiveStdinTerminalEvents() {
  if (!getStdinEnded() || _stdinLiveTerminalEventsEmitted || _stdinLiveBuffer.length > 0) {
    return;
  }
  if (_stdinLiveTerminalEventsScheduled) {
    return;
  }
  _stdinLiveTerminalEventsScheduled = true;
  queueMicrotask(() => {
    _stdinLiveTerminalEventsScheduled = false;
    if (!getStdinEnded() || _stdinLiveTerminalEventsEmitted || _stdinLiveBuffer.length > 0) {
      return;
    }
    _stdinLiveTerminalEventsEmitted = true;
    emitStdinListeners("end");
    emitStdinListeners("close");
    syncLiveStdinHandle(false);
  });
}

function finishLiveStdin() {
  if (getStdinEnded()) return;
  setStdinEnded(true);
  flushLiveStdinBuffer();
  maybeEmitLiveStdinTerminalEvents();
}

function _getStreamStdin() {
  return typeof __runtimeStreamStdin !== "undefined" && !!__runtimeStreamStdin;
}

function _getKernelStdin() {
  return typeof __runtimeKernelStdin !== "undefined" && !!__runtimeKernelStdin;
}

function ensureLiveStdinStarted() {
  if (_stdinLiveStarted) return;
  if (!_getStdinIsTTY() && !_getStreamStdin() && !_getKernelStdin()) return;
  _stdinLiveStarted = true;
  syncLiveStdinHandle(!_stdin.paused);
  if (_getStreamStdin() && !_getKernelStdin()) {
    return;
  }
  if (typeof _kernelStdinRead === "undefined") return;
  void (async () => {
    try {
      while (!getStdinEnded()) {
        if (typeof _kernelStdinRead === "undefined") {
          break;
        }
        const next = await _kernelStdinRead.apply(void 0, [65536, null], {
          result: { promise: true }
        });
        if (next?.done) {
          break;
        }
        const dataBase64 = String(next?.dataBase64 ?? "");
        if (!dataBase64) {
          continue;
        }
        _stdinLiveBuffer += _stdinLiveDecoder.decode(
          import_buffer2.Buffer.from(dataBase64, "base64"),
          { stream: true }
        );
        flushLiveStdinBuffer();
      }
    } catch {
    }
    _stdinLiveBuffer += _stdinLiveDecoder.decode();
    finishLiveStdin();
  })();
}

function stdinDispatch(eventType, payload) {
  if (eventType === "stdin_end") {
    finishLiveStdin();
    return;
  }
  if (eventType !== "stdin" || getStdinEnded()) {
    return;
  }
  let chunk: string;
  let binary = false;
  if (payload && typeof payload === "object" && typeof payload.dataBase64 === "string") {
    const bytes = import_buffer2.Buffer.from(payload.dataBase64, "base64");
    if (bytes.length === 0) {
      return;
    }
    if (!_stdin.encoding && getStdinFlowMode()) {
      emitStdinListeners("data", bytes);
      maybeEmitLiveStdinTerminalEvents();
      return;
    }
    chunk = _stdin.encoding ? bytes.toString(_stdin.encoding) : bytes.toString("latin1");
    binary = !_stdin.encoding;
  } else {
    chunk = typeof payload === "string" ? payload : payload == null ? "" : import_buffer2.Buffer.from(payload).toString("utf8");
  }
  if (!chunk) {
    return;
  }
  _stdinLiveBuffer += chunk;
  if (binary && !_stdin.encoding && getStdinFlowMode()) {
    const buffered = _stdinLiveBuffer;
    _stdinLiveBuffer = "";
    emitStdinListeners("data", import_buffer2.Buffer.from(buffered, "latin1"));
    maybeEmitLiveStdinTerminalEvents();
    return;
  }
  flushLiveStdinBuffer();
}

var _stdin = {
  readable: true,
  paused: true,
  encoding: null,
  isRaw: false,
  read(size) {
    if (_stdinLiveBuffer.length > 0) {
      if (!size || size >= _stdinLiveBuffer.length) {
        const chunk3 = _stdinLiveBuffer;
        _stdinLiveBuffer = "";
        return chunk3;
      }
      const chunk2 = _stdinLiveBuffer.slice(0, size);
      _stdinLiveBuffer = _stdinLiveBuffer.slice(size);
      return chunk2;
    }
    if (getStdinPosition() >= getStdinData().length) return null;
    const chunk = size ? getStdinData().slice(getStdinPosition(), getStdinPosition() + size) : getStdinData().slice(getStdinPosition());
    setStdinPosition(getStdinPosition() + chunk.length);
    return chunk;
  },
  on(event, listener) {
    if (!_stdinListeners[event]) _stdinListeners[event] = [];
    _stdinListeners[event].push(listener);
    if ((_getStdinIsTTY() || _getStreamStdin() || _getKernelStdin()) && (event === "data" || event === "end" || event === "close")) {
      ensureLiveStdinStarted();
    }
    if (event === "data" && this.paused) {
      this.resume();
    }
    if ((event === "end" || event === "close") && (_getStdinIsTTY() || _getStreamStdin() || _getKernelStdin())) {
      maybeEmitLiveStdinTerminalEvents();
    }
    if (event === "end" && getStdinData() && !getStdinEnded()) {
      setStdinFlowMode(true);
      _emitStdinData();
    }
    return this;
  },
  once(event, listener) {
    if (!_stdinOnceListeners[event]) _stdinOnceListeners[event] = [];
    _stdinOnceListeners[event].push(listener);
    if ((_getStdinIsTTY() || _getStreamStdin() || _getKernelStdin()) && (event === "data" || event === "end" || event === "close")) {
      ensureLiveStdinStarted();
    }
    if (event === "data" && this.paused) {
      this.resume();
    }
    if ((event === "end" || event === "close") && (_getStdinIsTTY() || _getStreamStdin() || _getKernelStdin())) {
      maybeEmitLiveStdinTerminalEvents();
    }
    if (event === "end" && getStdinData() && !getStdinEnded()) {
      setStdinFlowMode(true);
      _emitStdinData();
    }
    return this;
  },
  off(event, listener) {
    if (_stdinListeners[event]) {
      const idx = _stdinListeners[event].indexOf(listener);
      if (idx !== -1) _stdinListeners[event].splice(idx, 1);
    }
    return this;
  },
  removeListener(event, listener) {
    return this.off(event, listener);
  },
  emit(event, ...args) {
    const listeners = [..._stdinListeners[event] || [], ..._stdinOnceListeners[event] || []];
    _stdinOnceListeners[event] = [];
    for (const listener of listeners) {
      listener(args[0]);
    }
    return listeners.length > 0;
  },
  pause() {
    this.paused = true;
    setStdinFlowMode(false);
    syncLiveStdinHandle(false);
    return this;
  },
  resume() {
    if (_getStdinIsTTY() || _getStreamStdin() || _getKernelStdin()) {
      ensureLiveStdinStarted();
      syncLiveStdinHandle(true);
    }
    this.paused = false;
    setStdinFlowMode(true);
    flushLiveStdinBuffer();
    _emitStdinData();
    maybeEmitLiveStdinTerminalEvents();
    return this;
  },
  setEncoding(enc) {
    this.encoding = enc;
    return this;
  },
  setRawMode(mode) {
    if (!_getStdinIsTTY()) {
      throw new Error("setRawMode is not supported when stdin is not a TTY");
    }
    if (typeof _ptySetRawMode !== "undefined") {
      _ptySetRawMode.applySync(void 0, [mode]);
    }
    this.isRaw = mode;
    return this;
  },
  get isTTY() {
    return _getStdinIsTTY();
  },
  [Symbol.asyncIterator]: function() {
    const stream = this;
    const queuedChunks = [];
    const pendingResolves = [];
    let done = false;
    let error = null;
    const flush = () => {
      while (pendingResolves.length > 0) {
        if (error) {
          pendingResolves.shift()(Promise.reject(error));
          continue;
        }
        if (queuedChunks.length > 0) {
          pendingResolves.shift()(Promise.resolve({ done: false, value: queuedChunks.shift() }));
          continue;
        }
        if (done) {
          pendingResolves.shift()(Promise.resolve({ done: true, value: void 0 }));
          continue;
        }
        break;
      }
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
    stream.on("end", onEnd);
    stream.on("close", onEnd);
    stream.on("error", onError);
    stream.on("data", onData);
    stream.resume();
    return {
      next() {
        if (error) {
          return Promise.reject(error);
        }
        if (queuedChunks.length > 0) {
          return Promise.resolve({ done: false, value: queuedChunks.shift() });
        }
        if (done) {
          return Promise.resolve({ done: true, value: void 0 });
        }
        return new Promise((resolve) => {
          pendingResolves.push(resolve);
        });
      },
      return() {
        done = true;
        stream.off?.("data", onData);
        stream.off?.("end", onEnd);
        stream.off?.("close", onEnd);
        stream.off?.("error", onError);
        flush();
        return Promise.resolve({ done: true, value: void 0 });
      },
      [Symbol.asyncIterator]() {
        return this;
      }
    };
  }
};
export { STDIN_HANDLE_ID, _emitStdinData, _getKernelStdin, _getStreamStdin, _stdin, _stdinListeners, _stdinLiveBuffer, _stdinLiveDecoder, _stdinLiveHandleRegistered, _stdinLiveStarted, _stdinLiveTerminalEventsEmitted, _stdinLiveTerminalEventsScheduled, _stdinOnceListeners, configureLiveStdin, emitStdinListeners, ensureLiveStdinStarted, finishLiveStdin, flushLiveStdinBuffer, getStdinData, getStdinEnded, getStdinFlowMode, getStdinPosition, maybeEmitLiveStdinTerminalEvents, resetLiveStdinState, setStdinDataValue, setStdinEnded, setStdinFlowMode, setStdinPosition, stdinDispatch, syncLiveStdinHandle };
