function encodeDispatchArgs(args) {
  return JSON.stringify(
    args,
    (_key, value) => value === void 0 ? { __agentOsDispatchType: "undefined" } : value
  );
}
function encodeDispatch(method, args) {
  return `__bd:${method}:${encodeDispatchArgs(args)}`;
}
function parseDispatchResult(resultJson) {
  if (resultJson === null) {
    return void 0;
  }
  const parsed = JSON.parse(resultJson);
  if (parsed.__bd_error) {
    const error = new Error(parsed.__bd_error.message);
    error.name = parsed.__bd_error.name ?? "Error";
    if (parsed.__bd_error.code !== void 0) {
      error.code = parsed.__bd_error.code;
    }
    if (parsed.__bd_error.stack) {
      error.stack = parsed.__bd_error.stack;
    }
    throw error;
  }
  return parsed.__bd_result;
}
function requireDispatchBridge() {
  if (!_loadPolyfill) {
    throw new Error("_loadPolyfill is not available in sandbox");
  }
  return _loadPolyfill;
}
function bridgeDispatchSync(method, ...args) {
  const bridge = requireDispatchBridge();
  return parseDispatchResult(
    bridge.applySyncPromise(void 0, [encodeDispatch(method, args)])
  );
}
export { encodeDispatchArgs, encodeDispatch, parseDispatchResult, requireDispatchBridge, bridgeDispatchSync };
