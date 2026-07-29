import { once } from "./events.js";
import { exposeCustomGlobal } from "../global-exposure.js";
import { bridgeDispatchSync } from "../transport.js";
import { _exited, isProcessExitError, routeAsyncCallbackError, scheduleAsyncRethrow } from "./process.js";

var builtinTimersPromisesModule = {
  scheduler: {
    wait(delay = 0, options = void 0) {
      return builtinTimersPromisesModule.setTimeout(delay, void 0, options);
    },
    yield() {
      return builtinTimersPromisesModule.setImmediate();
    }
  },
  setImmediate(value = void 0, options = void 0) {
    if (options?.signal?.aborted) {
      return Promise.reject(options.signal.reason ?? new Error("The operation was aborted"));
    }
    return new Promise((resolve, reject) => {
      const onAbort = () => reject(options.signal.reason ?? new Error("The operation was aborted"));
      options?.signal?.addEventListener?.("abort", onAbort, { once: true });
      globalThis.setImmediate?.(() => {
        options?.signal?.removeEventListener?.("abort", onAbort);
        resolve(value);
      }) ?? globalThis.setTimeout?.(() => {
        options?.signal?.removeEventListener?.("abort", onAbort);
        resolve(value);
      }, 0);
    });
  },
  setInterval(delay = 1, value = void 0, options = void 0) {
    let active = true;
    const signal = options?.signal;
    if (signal?.aborted) {
      active = false;
    }
    return {
      [Symbol.asyncIterator]() {
        return this;
      },
      async next() {
        if (!active) {
          return { done: true, value: void 0 };
        }
        try {
          const intervalValue = await builtinTimersPromisesModule.setTimeout(delay, value, { signal });
          return active ? { done: false, value: intervalValue } : { done: true, value: void 0 };
        } catch (error) {
          active = false;
          throw error;
        }
      },
      async return() {
        active = false;
        return { done: true, value: void 0 };
      }
    };
  },
  setTimeout(delay = 1, value = void 0, options = void 0) {
    if (options?.signal?.aborted) {
      return Promise.reject(options.signal.reason ?? new Error("The operation was aborted"));
    }
    return new Promise((resolve, reject) => {
      const timer = globalThis.setTimeout?.(() => {
        options?.signal?.removeEventListener?.("abort", onAbort);
        resolve(value);
      }, delay ?? 0);
      const onAbort = () => {
        if (typeof globalThis.clearTimeout === "function") {
          globalThis.clearTimeout(timer);
        }
        reject(options.signal.reason ?? new Error("The operation was aborted"));
      };
      options?.signal?.addEventListener?.("abort", onAbort, { once: true });
    });
  }
};

var asyncLocalStorageInstances = /* @__PURE__ */ new Set();

function snapshotAsyncLocalStorageStores() {
  return Array.from(asyncLocalStorageInstances, (storage) => ({
    storage,
    hasStore: storage._hasStore === true,
    store: storage._store
  }));
}

function applyAsyncLocalStorageSnapshot(snapshot) {
  for (const entry of snapshot) {
    entry.storage._hasStore = entry.hasStore;
    entry.storage._store = entry.store;
  }
}

function runWithAsyncLocalStorageSnapshot(snapshot, callback, thisArg, args) {
  if (typeof callback !== "function") {
    return callback;
  }
  const previous = snapshotAsyncLocalStorageStores();
  applyAsyncLocalStorageSnapshot(snapshot);
  try {
    return callback.apply(thisArg, args);
  } finally {
    applyAsyncLocalStorageSnapshot(previous);
  }
}

function wrapAsyncLocalStorageCallback(callback, snapshot) {
  if (typeof callback !== "function") {
    return callback;
  }
  return function(...args) {
    try {
      return runWithAsyncLocalStorageSnapshot(snapshot, callback, this, args);
    } catch (error) {
      if (isProcessExitError(error)) {
        throw error;
      }
      throw error;
    }
  };
}

var builtinAsyncHooksModule = {
  AsyncLocalStorage: class {
    constructor() {
      this._hasStore = false;
      this._store = void 0;
      asyncLocalStorageInstances.add(this);
    }
    disable() {
      this._hasStore = false;
      this._store = void 0;
    }
    enterWith(store) {
      this._hasStore = true;
      this._store = store;
    }
    exit(callback, ...args) {
      const previous = {
        hasStore: this._hasStore,
        store: this._store
      };
      this._hasStore = false;
      this._store = void 0;
      let restoreOnFinally = true;
      try {
        const result = callback(...args);
        if (result && typeof result.then === "function") {
          restoreOnFinally = false;
          return Promise.resolve(result).finally(() => {
            this._hasStore = previous.hasStore;
            this._store = previous.store;
          });
        }
        return result;
      } finally {
        if (restoreOnFinally) {
          this._hasStore = previous.hasStore;
          this._store = previous.store;
        }
      }
    }
    getStore() {
      return this._hasStore ? this._store : void 0;
    }
    run(store, callback, ...args) {
      const previous = {
        hasStore: this._hasStore,
        store: this._store
      };
      this._hasStore = true;
      this._store = store;
      let restoreOnFinally = true;
      try {
        const result = callback(...args);
        if (result && typeof result.then === "function") {
          restoreOnFinally = false;
          return Promise.resolve(result).finally(() => {
            this._hasStore = previous.hasStore;
            this._store = previous.store;
          });
        }
        return result;
      } finally {
        if (restoreOnFinally) {
          this._hasStore = previous.hasStore;
          this._store = previous.store;
        }
      }
    }
  },
  AsyncResource: class {
    constructor(type = "AgentOsAsyncResource") {
      this.type = type;
      this._asyncLocalStorageSnapshot = snapshotAsyncLocalStorageStores();
    }
    emitBefore() {
    }
    emitAfter() {
    }
    emitDestroy() {
    }
    asyncId() {
      return 0;
    }
    triggerAsyncId() {
      return 0;
    }
    runInAsyncScope(callback, thisArg, ...args) {
      return runWithAsyncLocalStorageSnapshot(
        this._asyncLocalStorageSnapshot,
        callback,
        thisArg,
        args
      );
    }
  },
  createHook() {
    return {
      enable() {
        return this;
      },
      disable() {
        return this;
      }
    };
  },
  executionAsyncId() {
    return 0;
  },
  triggerAsyncId() {
    return 0;
  }
};

if (!Promise.prototype.__agentOSAsyncLocalStoragePatched) {
  const nativePromiseThen = Promise.prototype.then;
  Promise.prototype.then = function(onFulfilled, onRejected) {
    const snapshot = snapshotAsyncLocalStorageStores();
    const wrappedRejected = typeof onRejected === "function" ? (error) => {
      if (isProcessExitError(error)) {
        throw error;
      }
      return onRejected(error);
    } : onRejected;
    return nativePromiseThen.call(
      this,
      wrapAsyncLocalStorageCallback(onFulfilled, snapshot),
      wrapAsyncLocalStorageCallback(wrappedRejected, snapshot)
    );
  };
  Object.defineProperty(Promise.prototype, "__agentOSAsyncLocalStoragePatched", {
    value: true,
    configurable: true
  });
}

var TIMER_DISPATCH = {
  create: "kernelTimerCreate",
  arm: "kernelTimerArm",
  clear: "kernelTimerClear"
};

exposeCustomGlobal("_asyncHooksModule", builtinAsyncHooksModule);

var _queueMicrotask = typeof queueMicrotask === "function" ? queueMicrotask : function(fn) {
  Promise.resolve().then(fn);
};

function normalizeTimerDelay(delay) {
  const numericDelay = Number(delay ?? 0);
  if (!Number.isFinite(numericDelay) || numericDelay <= 0) {
    return 0;
  }
  return Math.floor(numericDelay);
}

function getTimerId(timer) {
  if (timer && typeof timer === "object" && timer._id !== void 0) {
    return timer._id;
  }
  if (typeof timer === "number") {
    return timer;
  }
  return void 0;
}

function createKernelTimer(delayMs, repeat) {
  try {
    return bridgeDispatchSync(TIMER_DISPATCH.create, delayMs, repeat);
  } catch (error) {
    if (error instanceof Error && error.message.includes("EAGAIN")) {
      throw new Error(
        "ERR_RESOURCE_BUDGET_EXCEEDED: maximum number of timers exceeded"
      );
    }
    throw error;
  }
}

function armKernelTimer(timerId) {
  bridgeDispatchSync(TIMER_DISPATCH.arm, timerId);
}

var TimerHandle = class {
  _id;
  _destroyed;
  _refed;
  constructor(id) {
    this._id = id;
    this._destroyed = false;
    this._refed = true;
  }
  ref() {
    this._refed = true;
    return this;
  }
  unref() {
    this._refed = false;
    return this;
  }
  hasRef() {
    return this._refed;
  }
  refresh() {
    if (!this._destroyed && _timerEntries.has(this._id)) {
      armKernelTimer(this._id);
    }
    return this;
  }
  [Symbol.toPrimitive]() {
    return this._id;
  }
};

var _timerEntries = /* @__PURE__ */ new Map();

var _immediateEntries = /* @__PURE__ */ new Map();

var _nextImmediateId = -1;

var _timerDrainResolvers = [];

function getRefedTimerCount() {
  if (typeof _exited !== "undefined" && _exited) {
    return 0;
  }
  let count = 0;
  for (const entry of _timerEntries.values()) {
    if (entry.handle?.hasRef?.() !== false) {
      count += 1;
    }
  }
  return count;
}

function getPendingImmediateCount() {
  if (typeof _exited !== "undefined" && _exited) {
    return 0;
  }
  return _immediateEntries.size;
}

function getPendingTimerDrainCount() {
  return getRefedTimerCount() + getPendingImmediateCount();
}

function checkTimerDrain() {
  if (getPendingTimerDrainCount() === 0 && _timerDrainResolvers.length > 0) {
    const resolvers = _timerDrainResolvers;
    _timerDrainResolvers = [];
    resolvers.forEach((r) => r());
  }
}

function _getPendingTimerCount() {
  return getPendingTimerDrainCount();
}

function _getPendingImmediateCount() {
  return getPendingImmediateCount();
}

function _waitForTimerDrain() {
  if (getPendingTimerDrainCount() === 0) return Promise.resolve();
  return new Promise((resolve) => {
    _timerDrainResolvers.push(resolve);
    checkTimerDrain();
  });
}

var _nextTickQueue = [];

var _nextTickScheduled = false;

function flushNextTickQueue() {
  _nextTickScheduled = false;
  while (_nextTickQueue.length > 0) {
    const entry = _nextTickQueue.shift();
    if (!entry) {
      break;
    }
    try {
      entry.callback(...entry.args);
    } catch (error) {
      const outcome = routeAsyncCallbackError(error);
      if (!outcome.handled && outcome.rethrow !== null) {
        _nextTickQueue.length = 0;
        scheduleAsyncRethrow(outcome.rethrow);
      }
      return;
    }
  }
}

function scheduleNextTickFlush() {
  if (_nextTickScheduled) {
    return;
  }
  _nextTickScheduled = true;
  const asyncLocalStorageSnapshot = snapshotAsyncLocalStorageStores();
  _queueMicrotask(() =>
    runWithAsyncLocalStorageSnapshot(
      asyncLocalStorageSnapshot,
      flushNextTickQueue,
      globalThis,
      []
    )
  );
}

function timerDispatch(_eventType, payload) {
  const timerId = typeof payload === "number" ? payload : Number(payload?.timerId);
  if (!Number.isFinite(timerId)) return;
  const entry = _timerEntries.get(timerId);
  if (!entry) return;
  if (!entry.repeat) {
    entry.handle._destroyed = true;
    _timerEntries.delete(timerId);
  }
  try {
    entry.callback(...entry.args);
  } catch (error) {
    const outcome = routeAsyncCallbackError(error);
    if (!outcome.handled && outcome.rethrow !== null) {
      throw outcome.rethrow;
    }
    return;
  }
  if (typeof _exited !== "undefined" && _exited) {
    checkTimerDrain();
    return;
  }
  if (entry.repeat && _timerEntries.has(timerId)) {
    armKernelTimer(timerId);
  }
  checkTimerDrain();
}

function setTimeout2(callback, delay, ...args) {
  const actualDelay = Math.max(1, normalizeTimerDelay(delay));
  const id = createKernelTimer(actualDelay, false);
  const handle = new TimerHandle(id);
  const asyncLocalStorageSnapshot = snapshotAsyncLocalStorageStores();
  _timerEntries.set(id, {
    handle,
    callback: wrapAsyncLocalStorageCallback(callback, asyncLocalStorageSnapshot),
    args,
    repeat: false
  });
  armKernelTimer(id);
  return handle;
}

function clearTimeout2(timer) {
  const id = getTimerId(timer);
  if (id === void 0) return;
  const entry = _timerEntries.get(id);
  if (entry) {
    entry.handle._destroyed = true;
    _timerEntries.delete(id);
  }
  bridgeDispatchSync(TIMER_DISPATCH.clear, id);
  checkTimerDrain();
}

function setInterval(callback, delay, ...args) {
  const actualDelay = Math.max(1, normalizeTimerDelay(delay));
  const id = createKernelTimer(actualDelay, true);
  const handle = new TimerHandle(id);
  const asyncLocalStorageSnapshot = snapshotAsyncLocalStorageStores();
  _timerEntries.set(id, {
    handle,
    callback: wrapAsyncLocalStorageCallback(callback, asyncLocalStorageSnapshot),
    args,
    repeat: true
  });
  armKernelTimer(id);
  return handle;
}

function clearInterval(timer) {
  clearTimeout2(timer);
}

exposeCustomGlobal("_timerDispatch", timerDispatch);

function _drainImmediates() {
  const entries = Array.from(_immediateEntries.entries());
  for (const [immediateId, entry] of entries) {
    if (_immediateEntries.get(immediateId) !== entry) {
      continue;
    }
    entry.handle._destroyed = true;
    _immediateEntries.delete(immediateId);
    try {
      entry.callback(...entry.args);
    } catch (error) {
      const outcome = routeAsyncCallbackError(error);
      if (!outcome.handled && outcome.rethrow !== null) {
        throw outcome.rethrow;
      }
      return;
    }
    if (typeof _exited !== "undefined" && _exited) {
      checkTimerDrain();
      return;
    }
  }
  checkTimerDrain();
}

exposeCustomGlobal("_getPendingTimerCount", _getPendingTimerCount);

exposeCustomGlobal("_drainImmediates", _drainImmediates);

exposeCustomGlobal("_getPendingImmediateCount", _getPendingImmediateCount);

exposeCustomGlobal("_waitForTimerDrain", _waitForTimerDrain);

function setImmediate(callback, ...args) {
  const id = _nextImmediateId--;
  const handle = new TimerHandle(id);
  const asyncLocalStorageSnapshot = snapshotAsyncLocalStorageStores();
  _immediateEntries.set(id, {
    handle,
    callback: wrapAsyncLocalStorageCallback(callback, asyncLocalStorageSnapshot),
    args,
    repeat: false
  });
  return handle;
}

function clearImmediate(timer) {
  const id = getTimerId(timer);
  if (id === void 0) return;
  const entry = _immediateEntries.get(id);
  if (entry) {
    entry.handle._destroyed = true;
    _immediateEntries.delete(id);
    checkTimerDrain();
    return;
  }
  if (id < 0) return;
  clearTimeout2(timer);
}
export { TIMER_DISPATCH, TimerHandle, _drainImmediates, _getPendingImmediateCount, _getPendingTimerCount, _immediateEntries, _nextImmediateId, _nextTickQueue, _nextTickScheduled, _queueMicrotask, _timerDrainResolvers, _timerEntries, _waitForTimerDrain, applyAsyncLocalStorageSnapshot, armKernelTimer, asyncLocalStorageInstances, builtinAsyncHooksModule, builtinTimersPromisesModule, checkTimerDrain, clearImmediate, clearInterval, clearTimeout2, createKernelTimer, flushNextTickQueue, getPendingImmediateCount, getPendingTimerDrainCount, getRefedTimerCount, getTimerId, normalizeTimerDelay, runWithAsyncLocalStorageSnapshot, scheduleNextTickFlush, setImmediate, setInterval, setTimeout2, snapshotAsyncLocalStorageStores, timerDispatch, wrapAsyncLocalStorageCallback };
