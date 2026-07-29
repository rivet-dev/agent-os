import { _exited } from "./process.js";
import { exposeCustomGlobal } from "../global-exposure.js";
import { bridgeDispatchSync } from "../transport.js";

var HANDLE_DISPATCH = {
  register: "kernelHandleRegister",
  unregister: "kernelHandleUnregister",
  list: "kernelHandleList"
};
var _activeHandles = /* @__PURE__ */ new Map();
var _waitResolvers = [];

var _activeHandleDrainScheduled = false;

function scheduleActiveHandleDrain() {
  if (_activeHandleDrainScheduled) return;
  _activeHandleDrainScheduled = true;
  const defer =
    typeof globalThis.setImmediate === "function"
      ? globalThis.setImmediate
      : queueMicrotask;
  defer(() => {
    _activeHandleDrainScheduled = false;
    if (_activeHandles.size !== 0 || _waitResolvers.length === 0) return;
    const resolvers = _waitResolvers;
    _waitResolvers = [];
    for (const resolve of resolvers) resolve();
  });
}

function _registerHandle2(id, description) {
  try {
    bridgeDispatchSync(HANDLE_DISPATCH.register, id, description);
  } catch (error) {
    if (error instanceof Error && error.message.includes("EAGAIN")) {
      throw new Error(
        "ERR_RESOURCE_BUDGET_EXCEEDED: maximum active handles exceeded"
      );
    }
    throw error;
  }
  _activeHandles.set(id, description);
}
function _unregisterHandle2(id) {
  _activeHandles.delete(id);
  const remaining = _activeHandles.size;
  try {
    bridgeDispatchSync(HANDLE_DISPATCH.unregister, id);
  } catch {
  }
  if (remaining === 0 && _waitResolvers.length > 0) scheduleActiveHandleDrain();
}
function _waitForActiveHandles() {
  if (typeof _exited !== "undefined" && _exited) {
    return Promise.resolve();
  }
  const getPendingTimerCount = globalThis._getPendingTimerCount;
  const waitForTimerDrain = globalThis._waitForTimerDrain;
  const hasHandles = _getActiveHandles().length > 0;
  const hasTimers =
    typeof getPendingTimerCount === "function" && getPendingTimerCount() > 0;
  if (!hasHandles && !hasTimers) {
    return Promise.resolve();
  }
  const promises = [];
  if (hasHandles) {
    promises.push(
      new Promise((resolve) => {
        let settled = false;
        const complete = () => {
          if (settled) return;
          settled = true;
          resolve();
        };
        _waitResolvers.push(complete);
        if (_getActiveHandles().length === 0) {
          complete();
        }
      })
    );
  }
  if (hasTimers && typeof waitForTimerDrain === "function") {
    promises.push(waitForTimerDrain());
  }
  return Promise.all(promises).then(() => {
    const timersRemain =
      typeof getPendingTimerCount === "function" && getPendingTimerCount() > 0;
    if (_getActiveHandles().length > 0 || timersRemain) {
      return _waitForActiveHandles();
    }
  });
}
function _getActiveHandles() {
  return Array.from(_activeHandles.values());
}
exposeCustomGlobal("_registerHandle", _registerHandle2);
exposeCustomGlobal("_unregisterHandle", _unregisterHandle2);
exposeCustomGlobal("_waitForActiveHandles", _waitForActiveHandles);
exposeCustomGlobal("_getActiveHandles", _getActiveHandles);
export { HANDLE_DISPATCH, _activeHandles, _waitResolvers, _registerHandle2, _unregisterHandle2, _waitForActiveHandles, _getActiveHandles };
