var VM_CONTEXT_TAG = typeof Symbol === "function" ? Symbol.for("agentos.vm.context") : "__agentos_vm_context__";

var VM_CONTEXT_ID = typeof Symbol === "function" ? Symbol.for("agentos.vm.context.id") : "__agentos_vm_context_id__";

function createVmNotImplementedError(feature) {
  const error = new Error(`node:vm ${feature} is not implemented in the agentos guest runtime`);
  error.code = "ERR_NOT_IMPLEMENTED";
  return error;
}

function isVmContextCandidate(value) {
  return value !== null && (typeof value === "object" || typeof value === "function");
}

function normalizeVmOptions(options = void 0) {
  if (typeof options === "string") {
    return { filename: options };
  }
  if (!options || typeof options !== "object") {
    return {};
  }
  const normalized = {};
  if (typeof options.filename === "string") {
    normalized.filename = options.filename;
  }
  if (Number.isInteger(options.lineOffset)) {
    normalized.lineOffset = options.lineOffset;
  }
  if (Number.isInteger(options.columnOffset)) {
    normalized.columnOffset = options.columnOffset;
  }
  if (Number.isInteger(options.timeout) && options.timeout > 0) {
    normalized.timeout = options.timeout;
  }
  if (options.cachedData !== void 0) {
    normalized.cachedData = options.cachedData;
  }
  if (options.produceCachedData === true) {
    normalized.produceCachedData = true;
  }
  return normalized;
}

function mergeVmOptions(baseOptions, overrideOptions) {
  const base = normalizeVmOptions(baseOptions);
  const override = normalizeVmOptions(overrideOptions);
  return { ...base, ...override };
}

function vmCreateContext(context = {}) {
  if (!isVmContextCandidate(context)) {
    throw new TypeError('The "object" argument must be of type object.');
  }
  if (context[VM_CONTEXT_TAG] === true && Number.isInteger(context[VM_CONTEXT_ID])) {
    return context;
  }
  const contextId = _vmCreateContext(context);
  Object.defineProperty(context, VM_CONTEXT_TAG, {
    value: true,
    configurable: true,
    enumerable: false,
    writable: false
  });
  Object.defineProperty(context, VM_CONTEXT_ID, {
    value: contextId,
    configurable: false,
    enumerable: false,
    writable: false
  });
  return context;
}

function vmIsContext(context) {
  return isVmContextCandidate(context) && context[VM_CONTEXT_TAG] === true && Number.isInteger(context[VM_CONTEXT_ID]);
}

function assertVmContext(context) {
  if (!vmIsContext(context)) {
    throw new TypeError('The "contextifiedObject" argument must be a vm context.');
  }
  return context;
}

function vmRunInThisContext(code, options = void 0) {
  return _vmRunInThisContext(String(code), normalizeVmOptions(options));
}

function vmRunInContext(code, contextifiedObject, options = void 0) {
  const context = assertVmContext(contextifiedObject);
  return _vmRunInContext(context[VM_CONTEXT_ID], String(code), normalizeVmOptions(options), context);
}

function vmRunInNewContext(code, contextOrOptions = {}, maybeOptions = void 0) {
  const hasExplicitContext = isVmContextCandidate(contextOrOptions);
  const context = hasExplicitContext ? contextOrOptions : {};
  const options = hasExplicitContext ? maybeOptions : contextOrOptions;
  return vmRunInContext(code, vmCreateContext(context), options);
}

class VmScript {
  constructor(code, options = void 0) {
    this.code = String(code);
    this.options = normalizeVmOptions(options);
    this.filename = this.options.filename ?? "evalmachine.<anonymous>";
    this.lineOffset = this.options.lineOffset ?? 0;
    this.columnOffset = this.options.columnOffset ?? 0;
    this.cachedData = this.options.cachedData;
    this.cachedDataProduced = false;
    this.cachedDataRejected = false;
  }
  createCachedData() {
    return typeof Buffer === "function" ? Buffer.alloc(0) : new Uint8Array(0);
  }
  runInThisContext(options = void 0) {
    return vmRunInThisContext(this.code, mergeVmOptions(this.options, options));
  }
  runInContext(contextifiedObject, options = void 0) {
    return vmRunInContext(this.code, contextifiedObject, mergeVmOptions(this.options, options));
  }
  runInNewContext(context = {}, options = void 0) {
    return vmRunInNewContext(this.code, context, mergeVmOptions(this.options, options));
  }
}

var builtinVmModule = {
  Script: VmScript,
  compileFunction() {
    throw createVmNotImplementedError("compileFunction");
  },
  createContext: vmCreateContext,
  isContext: vmIsContext,
  measureMemory() {
    throw createVmNotImplementedError("measureMemory");
  },
  runInContext: vmRunInContext,
  runInNewContext: vmRunInNewContext,
  runInThisContext: vmRunInThisContext
};
export { VM_CONTEXT_ID, VM_CONTEXT_TAG, VmScript, assertVmContext, builtinVmModule, createVmNotImplementedError, isVmContextCandidate, mergeVmOptions, normalizeVmOptions, vmCreateContext, vmIsContext, vmRunInContext, vmRunInNewContext, vmRunInThisContext };
