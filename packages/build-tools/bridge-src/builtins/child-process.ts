import { _fdClose, _fdGetPath, encodeBridgeBytes, fs } from "./fs.js";
import { normalizeChildProcessSignal } from "./os.js";
import { exposeCustomGlobal } from "../global-exposure.js";
import { __export } from "../vendor/esbuild-runtime.js";

var child_process_exports = {};
__export(child_process_exports, {
  ChildProcess: () => ChildProcess,
  default: () => child_process_default,
  exec: () => exec,
  execFile: () => execFile,
  execFileSync: () => execFileSync,
  execSync: () => execSync,
  fork: () => fork,
  spawn: () => spawn,
  spawnSync: () => spawnSync
});
var childProcessInstances = /* @__PURE__ */ new Map();
var earlyChildProcessEvents = /* @__PURE__ */ new Map();
const MAX_EARLY_CHILD_PROCESS_IDS = 64;
const MAX_EARLY_CHILD_PROCESS_EVENTS = 256;
const CHILD_PROCESS_EXIT_DRAIN_MAX_MS = 1_000;
const CHILD_PROCESS_EVENT_ROUTES = Symbol.for("agentos.childProcessEventRoutes");
const childProcessEventRoutes = (() => {
  const existing = globalThis[CHILD_PROCESS_EVENT_ROUTES];
  if (existing instanceof Map) return existing;
  const routes = /* @__PURE__ */ new Map();
  Object.defineProperty(globalThis, CHILD_PROCESS_EVENT_ROUTES, {
    value: routes,
    configurable: false,
    enumerable: false,
    writable: false
  });
  return routes;
})();
function publishChildProcessEvent(eventType, payload) {
  const route = childProcessEventRoutes.get(payload?.sessionId);
  if (typeof route !== "function") return;
  try {
    route(eventType, payload);
  } catch (error) {
    queueMicrotask(() => {
      throw error;
    });
  }
}
// fds handed to a live child as its inherited stdout/stderr. Node keeps the
// underlying file open for the child's lifetime even after the parent closes
// its own descriptor (the child dup'd it at fork). We emulate that: the parent's
// fs.closeSync on such an fd is deferred until the child exits, so async child
// output can still be written to the fd. Per fd we track the number of live
// children holding it and whether the parent already requested a close.
var _childInheritedFds = /* @__PURE__ */ new Map();
function retainChildInheritedFd(fd, closeOnRelease = false) {
  if (typeof fd !== "number") return;
  const entry = _childInheritedFds.get(fd);
  if (entry) {
    entry.holders += 1;
    entry.closePending ||= closeOnRelease;
  } else {
    _childInheritedFds.set(fd, { holders: 1, closePending: closeOnRelease });
  }
}
function deferCloseIfChildInheritedFd(fd) {
  const entry = _childInheritedFds.get(fd);
  if (!entry) return false;
  entry.closePending = true;
  return true;
}
function releaseChildInheritedFd(fd) {
  const entry = _childInheritedFds.get(fd);
  if (!entry) return;
  entry.holders -= 1;
  if (entry.holders > 0) return;
  _childInheritedFds.delete(fd);
  if (entry.closePending) {
    try {
      _fdClose.applySyncPromise(void 0, [fd]);
    } catch {
    }
  }
}
function childInheritedFdPath(fd) {
  if (typeof fd !== "number") return null;
  try {
    const path = _fdGetPath.applySyncPromise(void 0, [fd]);
    return typeof path === "string" && path.startsWith("/") ? path : null;
  } catch {
    return null;
  }
}
function normalizeChildProcessSessionId(payload) {
  if (!payload || typeof payload !== "object") {
    return null;
  }
  if (typeof payload.sessionId === "string" && payload.sessionId.length > 0) {
    return payload.sessionId;
  }
  if (typeof payload.sessionId === "number" && Number.isFinite(payload.sessionId)) {
    return payload.sessionId;
  }
  return null;
}
function normalizeChildProcessBridgePayload(payload) {
  if (payload && typeof payload === "object") {
    return payload;
  }
  if (typeof payload === "string") {
    try {
      const parsed = JSON.parse(payload);
      return parsed && typeof parsed === "object" ? parsed : payload;
    } catch {
    }
  }
  return payload;
}
const CHILD_PROCESS_IPC_FRAME_PREFIX = "\x1EAGENTOS_IPC:";
const CHILD_PROCESS_IPC_MAX_GRAPH_NODES = 65536;
const CHILD_PROCESS_IPC_MAX_GRAPH_DEPTH = 512;
function createIpcSerializationLimitError(limit) {
  const error = new Error(`ERR_RESOURCE_BUDGET_EXCEEDED: advanced child_process IPC message exceeds ${limit}`);
  error.code = "ERR_RESOURCE_BUDGET_EXCEEDED";
  return error;
}
function encodeAdvancedIpcMessage(message) {
  const seen = /* @__PURE__ */ new Map();
  const nodes = [];
  function encode(value, depth) {
    if (depth > CHILD_PROCESS_IPC_MAX_GRAPH_DEPTH) {
      throw createIpcSerializationLimitError(`maximum graph depth ${CHILD_PROCESS_IPC_MAX_GRAPH_DEPTH}`);
    }
    if (value === null || typeof value === "string" || typeof value === "boolean") return value;
    if (typeof value === "number") {
      if (Number.isNaN(value)) return { $t: "nan" };
      if (value === Infinity) return { $t: "+inf" };
      if (value === -Infinity) return { $t: "-inf" };
      if (Object.is(value, -0)) return { $t: "-0" };
      return value;
    }
    if (typeof value === "undefined") return { $t: "undefined" };
    if (typeof value === "bigint") return { $t: "bigint", value: String(value) };
    if (typeof value === "function" || typeof value === "symbol") {
      const error = new TypeError(`${typeof value} could not be cloned by advanced child_process IPC`);
      error.code = "ERR_IPC_MESSAGE_SERIALIZATION";
      throw error;
    }
    const existing = seen.get(value);
    if (existing !== undefined) return { $r: existing };
    if (nodes.length >= CHILD_PROCESS_IPC_MAX_GRAPH_NODES) {
      throw createIpcSerializationLimitError(`maximum graph nodes ${CHILD_PROCESS_IPC_MAX_GRAPH_NODES}`);
    }
    const index = nodes.length;
    seen.set(value, index);
    nodes.push(null);
    let node;
    if (Array.isArray(value)) {
      node = { type: "array", values: value.map((entry) => encode(entry, depth + 1)) };
    } else if (value instanceof Date) {
      node = { type: "date", value: value.toISOString() };
    } else if (value instanceof RegExp) {
      node = { type: "regexp", source: value.source, flags: value.flags, lastIndex: value.lastIndex };
    } else if (typeof Buffer !== "undefined" && Buffer.isBuffer(value)) {
      node = { type: "buffer", value: value.toString("base64") };
    } else if (value instanceof ArrayBuffer) {
      const bytes = new Uint8Array(value);
      node = { type: "arraybuffer", value: typeof Buffer !== "undefined" ? Buffer.from(bytes).toString("base64") : btoa(String.fromCharCode(...bytes)) };
    } else if (ArrayBuffer.isView(value)) {
      const bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
      node = {
        type: "typedarray",
        name: value.constructor?.name || "Uint8Array",
        value: typeof Buffer !== "undefined" ? Buffer.from(bytes).toString("base64") : btoa(String.fromCharCode(...bytes))
      };
    } else if (value instanceof Map) {
      node = { type: "map", values: Array.from(value, ([key, entry]) => [encode(key, depth + 1), encode(entry, depth + 1)]) };
    } else if (value instanceof Set) {
      node = { type: "set", values: Array.from(value, (entry) => encode(entry, depth + 1)) };
    } else if (value instanceof Error) {
      node = {
        type: "error",
        name: value.name,
        message: value.message,
        stack: value.stack,
        values: Object.keys(value).map((key) => [key, encode(value[key], depth + 1)])
      };
    } else {
      node = { type: "object", values: Object.keys(value).map((key) => [key, encode(value[key], depth + 1)]) };
    }
    nodes[index] = node;
    return { $r: index };
  }
  return JSON.stringify({ __agentOSAdvancedIpc: 1, root: encode(message, 0), nodes });
}
function decodeAdvancedIpcMessage(envelope) {
  if (!Array.isArray(envelope.nodes) || envelope.nodes.length > CHILD_PROCESS_IPC_MAX_GRAPH_NODES) {
    throw createIpcSerializationLimitError(`maximum graph nodes ${CHILD_PROCESS_IPC_MAX_GRAPH_NODES}`);
  }
  const shells = envelope.nodes.map((node) => {
    switch (node?.type) {
      case "array": return [];
      case "date": return new Date(node.value);
      case "regexp": {
        const value = new RegExp(node.source, node.flags);
        value.lastIndex = node.lastIndex || 0;
        return value;
      }
      case "buffer": return typeof Buffer !== "undefined" ? Buffer.from(node.value, "base64") : Uint8Array.from(atob(node.value), (character) => character.charCodeAt(0));
      case "arraybuffer": {
        const bytes = typeof Buffer !== "undefined" ? Buffer.from(node.value, "base64") : Uint8Array.from(atob(node.value), (character) => character.charCodeAt(0));
        return Uint8Array.from(bytes).buffer;
      }
      case "typedarray": {
        const bytes = typeof Buffer !== "undefined" ? Buffer.from(node.value, "base64") : Uint8Array.from(atob(node.value), (character) => character.charCodeAt(0));
        const ctor = globalThis[node.name];
        const buffer = Uint8Array.from(bytes).buffer;
        return typeof ctor === "function" && ctor.BYTES_PER_ELEMENT ? new ctor(buffer) : new Uint8Array(buffer);
      }
      case "map": return /* @__PURE__ */ new Map();
      case "set": return /* @__PURE__ */ new Set();
      case "error": {
        const value = new Error(node.message);
        value.name = node.name || "Error";
        if (node.stack !== undefined) value.stack = node.stack;
        return value;
      }
      default: return {};
    }
  });
  function decode(token, depth) {
    if (depth > CHILD_PROCESS_IPC_MAX_GRAPH_DEPTH) {
      throw createIpcSerializationLimitError(`maximum graph depth ${CHILD_PROCESS_IPC_MAX_GRAPH_DEPTH}`);
    }
    if (token === null || typeof token !== "object") return token;
    if (Object.prototype.hasOwnProperty.call(token, "$r")) {
      if (!Number.isInteger(token.$r) || token.$r < 0 || token.$r >= shells.length) {
        throw new TypeError("invalid advanced child_process IPC reference");
      }
      return shells[token.$r];
    }
    switch (token.$t) {
      case "undefined": return undefined;
      case "nan": return NaN;
      case "+inf": return Infinity;
      case "-inf": return -Infinity;
      case "-0": return -0;
      case "bigint": return BigInt(token.value);
      default: return token;
    }
  }
  for (let index = 0; index < envelope.nodes.length; index += 1) {
    const node = envelope.nodes[index];
    const shell = shells[index];
    if (node.type === "array") {
      for (const value of node.values) shell.push(decode(value, 1));
    } else if (node.type === "object" || node.type === "error") {
      for (const [key, value] of node.values || []) shell[key] = decode(value, 1);
    } else if (node.type === "map") {
      for (const [key, value] of node.values) shell.set(decode(key, 1), decode(value, 1));
    } else if (node.type === "set") {
      for (const value of node.values) shell.add(decode(value, 1));
    }
  }
  return decode(envelope.root, 0);
}
function encodeChildProcessIpcFrame(message, serialization) {
  const mode = serialization ?? globalThis.__agentOSProcessConfigEnv?.AGENTOS_NODE_IPC_SERIALIZATION;
  const json = mode === "advanced" ? encodeAdvancedIpcMessage(message) : JSON.stringify(message);
  const encoded = typeof Buffer !== "undefined" ? Buffer.from(json, "utf8").toString("base64") : btoa(json);
  return `${CHILD_PROCESS_IPC_FRAME_PREFIX}${encoded}\n`;
}
function decodeChildProcessIpcFramePayload(payload) {
  const json = typeof Buffer !== "undefined" ? Buffer.from(payload, "base64").toString("utf8") : atob(payload);
  const parsed = JSON.parse(json);
  return parsed?.__agentOSAdvancedIpc === 1 ? decodeAdvancedIpcMessage(parsed) : parsed;
}
function splitChildProcessIpcFrames(buffer, chunk) {
  const text = `${buffer}${typeof Buffer !== "undefined" ? Buffer.from(chunk).toString("utf8") : String(chunk)}`;
  const messages = [];
  const output = [];
  let cursor = 0;
  while (true) {
    const frameStart = text.indexOf(CHILD_PROCESS_IPC_FRAME_PREFIX, cursor);
    if (frameStart === -1) {
      output.push(text.slice(cursor));
      return { buffer: "", messages, output: output.join("") };
    }
    output.push(text.slice(cursor, frameStart));
    const payloadStart = frameStart + CHILD_PROCESS_IPC_FRAME_PREFIX.length;
    const frameEnd = text.indexOf("\n", payloadStart);
    if (frameEnd === -1) {
      return { buffer: text.slice(frameStart), messages, output: output.join("") };
    }
    try {
      messages.push(decodeChildProcessIpcFramePayload(text.slice(payloadStart, frameEnd)));
    } catch (error) {
      output.push(text.slice(frameStart, frameEnd + 1));
    }
    cursor = frameEnd + 1;
  }
}
// When a child stdout/stderr is wired to an inherited numeric fd, write the
// bytes straight to that descriptor (matching native node, where the child's
// output lands in the inherited file/pipe rather than on child.stdout). Returns
// true when the data was consumed by the fd so the caller skips stream emission.
function writeChildOutputToInheritedFd(fd, buf, path = null) {
  const bytes = typeof Buffer !== "undefined" && Buffer.isBuffer(buf) ? buf : typeof Buffer !== "undefined" ? Buffer.from(buf) : buf;
  if (typeof path === "string") {
    try {
      fs.appendFileSync(path, bytes);
      return true;
    } catch {
    }
  }
  if (typeof fd !== "number") return false;
  try {
    fs.writeSync(fd, bytes, 0, bytes.length, null);
  } catch {
    return false;
  }
  return true;
}
// Sync-path (spawnSync/execSync/execFileSync) fd inheritance: write the already
// captured output value (string or Buffer) to the inherited descriptor.
function redirectSyncOutputToInheritedFd(fd, output) {
  if (typeof fd !== "number" || output == null) return false;
  try {
    const bytes = typeof output === "string" ? (typeof Buffer !== "undefined" ? Buffer.from(output) : output) : typeof Buffer !== "undefined" && Buffer.isBuffer(output) ? output : typeof Buffer !== "undefined" ? Buffer.from(output) : output;
    fs.writeSync(fd, bytes, 0, bytes.length, null);
  } catch {
  }
  return true;
}
function routeChildProcessEvent(sessionId, type, data) {
  const child = childProcessInstances.get(sessionId);
  if (!child) {
    let events = earlyChildProcessEvents.get(sessionId);
    if (!events) {
      if (earlyChildProcessEvents.size >= MAX_EARLY_CHILD_PROCESS_IDS) {
        earlyChildProcessEvents.delete(earlyChildProcessEvents.keys().next().value);
      }
      events = [];
      earlyChildProcessEvents.set(sessionId, events);
    }
    if (events.length < MAX_EARLY_CHILD_PROCESS_EVENTS) {
      events.push({ type, data });
    }
    return;
  }
  if (type === "stdout") {
    const buf = typeof Buffer !== "undefined" ? Buffer.from(data) : data;
    if (child._ipcEnabled) {
      const parsed = splitChildProcessIpcFrames(child._ipcStdoutBuffer, buf);
      child._ipcStdoutBuffer = parsed.buffer;
      for (const message of parsed.messages) {
        if (message?.__agentOSControl === "ipc-ready") {
          child._ipcReady = true;
          child._flushIpcOutboundQueue();
          continue;
        }
        child._emitOrQueueIpcMessage(message);
      }
      if (parsed.output.length === 0) {
        return;
      }
      const outBuf = typeof Buffer !== "undefined" ? Buffer.from(parsed.output, "utf8") : parsed.output;
      if (writeChildOutputToInheritedFd(child._stdoutFd, outBuf, child._stdoutPath)) return;
      child.stdout.emit("data", outBuf);
      return;
    }
    if (writeChildOutputToInheritedFd(child._stdoutFd, buf, child._stdoutPath)) return;
    child.stdout.emit("data", buf);
  } else if (type === "stderr") {
    const buf = typeof Buffer !== "undefined" ? Buffer.from(data) : data;
    if (writeChildOutputToInheritedFd(child._stderrFd, buf, child._stderrPath)) return;
    child.stderr.emit("data", buf);
  } else if (type === "exit") {
    const signalCode = data && typeof data === "object" ? data.signal ?? null : null;
    const exitCode = data && typeof data === "object" ? data.code : data;
    if (child._exitScheduled) return;
    child._exitScheduled = true;
    const drainDeadline = Date.now() + CHILD_PROCESS_EXIT_DRAIN_MAX_MS;
    const finalizeExit = () => {
      // Effect's Node stream adapter consumes child output through the paused
      // `readable`/`read()` contract. Let that registered consumer drain bytes
      // already delivered before publishing exit: otherwise the process scope
      // can close its stream fiber while those bytes are still buffered. This
      // normally completes on the next turn and adds no fixed exit delay.
      const waitingForReadableConsumer = [child.stdout, child.stderr].some(
        (stream) =>
          stream._bufferedChunks.length > 0 &&
          hasOutputListeners(stream, "readable"),
      );
      if (waitingForReadableConsumer && Date.now() < drainDeadline) {
        scheduleOutputFlush(child.stdout);
        scheduleOutputFlush(child.stderr);
        setTimeout(finalizeExit, 0);
        return;
      }
      if (waitingForReadableConsumer && typeof console !== "undefined") {
        console.error(
          `ERR_AGENTOS_CHILD_STDIO_DRAIN_TIMEOUT: child ${sessionId} output was not consumed within ${CHILD_PROCESS_EXIT_DRAIN_MAX_MS}ms`,
        );
      }
      const wasConnected = child.connected;
      child.connected = false;
      child._pendingSignalCode = null;
      child.signalCode = signalCode;
      child.exitCode = signalCode == null ? exitCode : null;
      child.stdin.writable = false;
      child.stdin.destroyed = true;
      if (wasConnected) child.emit("disconnect");
      if (Array.isArray(child._inheritedFds)) {
        for (const fd of child._inheritedFds) releaseChildInheritedFd(fd);
        child._inheritedFds = [];
      }
      // Native stdout/stderr reach EOF before `close`, and consumers such as
      // Effect finish their stream fiber from that EOF. Publish it before the
      // process exit callback and yield one turn so exit cannot close the
      // consumer's scope while its final chunk is still being reduced.
      child.stdout.emit("end");
      child.stderr.emit("end");
      const publishExit = () => {
        // A consumer can attach after finalizeExit's first drain check (Effect
        // does this while resuming the spawn fiber). Recheck immediately before
        // close so that attaching during this window cannot strand buffered
        // output and complete the command with an empty result.
        const waitingForLateReadableConsumer = [child.stdout, child.stderr].some(
          (stream) =>
            stream._bufferedChunks.length > 0 &&
            hasOutputListeners(stream, "readable"),
        );
        if (waitingForLateReadableConsumer && Date.now() < drainDeadline) {
          scheduleOutputFlush(child.stdout);
          scheduleOutputFlush(child.stderr);
          setTimeout(publishExit, 0);
          return;
        }
        if (waitingForLateReadableConsumer && typeof console !== "undefined") {
          console.error(
            `ERR_AGENTOS_CHILD_STDIO_DRAIN_TIMEOUT: child ${sessionId} output was not consumed within ${CHILD_PROCESS_EXIT_DRAIN_MAX_MS}ms`,
          );
        }
        child.emit("exit", child.exitCode, child.signalCode);
        child.emit("close", child.exitCode, child.signalCode);
        childProcessInstances.delete(sessionId);
        if (typeof _unregisterHandle === "function") {
          _unregisterHandle(`child:${sessionId}`);
        }
      };
      // EOF listeners may resume their consumer through a task (Effect's Node
      // stream adapter does this). A microtask can still publish exit first and
      // close that consumer's scope before it commits the final chunk/result.
      setTimeout(publishExit, 0);
    };
    // Stream callbacks can hand a chunk to an async consumer (notably Effect's
    // Node stream adapter) without leaving it in `_bufferedChunks`. Give that
    // consumer one event-loop turn before EOF/exit closes its scope. A
    // microtask is too early because the consumer resumes through the runtime's
    // task queue; this zero-delay turn preserves Node's stdout-before-close
    // contract without imposing a fixed drain delay on every subprocess.
    setTimeout(finalizeExit, 0);
  }
}
var childProcessDispatch = (eventTypeOrSessionId, payloadOrType, data) => {
  if (typeof eventTypeOrSessionId === "number") {
    routeChildProcessEvent(
      eventTypeOrSessionId,
      payloadOrType,
      data
    );
    return;
  }
  const payload = (() => {
    if (payloadOrType && typeof payloadOrType === "object") {
      return payloadOrType;
    }
    if (typeof payloadOrType === "string") {
      try {
        return JSON.parse(payloadOrType);
      } catch {
        return null;
      }
    }
    return null;
  })();
  const sessionId = normalizeChildProcessSessionId(payload);
  if (sessionId == null) {
    return;
  }
  if (eventTypeOrSessionId === "child_stdout" || eventTypeOrSessionId === "child_stderr") {
    const directData = payload?.data;
    let bytes;
    if (typeof Buffer !== "undefined" && Buffer.isBuffer(directData)) {
      bytes = Buffer.from(directData);
    } else if (directData instanceof Uint8Array) {
      bytes = typeof Buffer !== "undefined" ? Buffer.from(directData.buffer, directData.byteOffset, directData.byteLength) : directData;
    } else if (ArrayBuffer.isView(directData)) {
      bytes = typeof Buffer !== "undefined" ? Buffer.from(directData.buffer, directData.byteOffset, directData.byteLength) : new Uint8Array(directData.buffer, directData.byteOffset, directData.byteLength);
    } else {
      const encoded = typeof payload?.dataBase64 === "string" ? payload.dataBase64 : typeof directData === "string" ? directData : directData?.__agentOSType === "bytes" && typeof directData?.base64 === "string" ? directData.base64 : "";
      bytes = typeof Buffer !== "undefined" ? Buffer.from(encoded, "base64") : new Uint8Array(
        atob(encoded).split("").map((char) => char.charCodeAt(0))
      );
    }
    routeChildProcessEvent(
      sessionId,
      eventTypeOrSessionId === "child_stdout" ? "stdout" : "stderr",
      bytes
    );
    publishChildProcessEvent(eventTypeOrSessionId, {
      sessionId,
      data: bytes
    });
    return;
  }
  if (eventTypeOrSessionId === "child_exit") {
    const code = typeof payload?.code === "number" ? payload.code : Number(payload?.code ?? 1);
    const signal = typeof payload?.signal === "string" ? payload.signal : null;
    routeChildProcessEvent(sessionId, "exit", { code, signal });
    publishChildProcessEvent(eventTypeOrSessionId, { sessionId, code, signal });
  }
};
exposeCustomGlobal("_childProcessDispatch", childProcessDispatch);
function hasOutputListeners(stream, event) {
  return (stream._listeners[event]?.length ?? 0) > 0 || (stream._onceListeners[event]?.length ?? 0) > 0;
}
// Node Readable fidelity: when setEncoding(enc) is configured on a child
// stdout/stderr stream, `data` chunks are delivered as strings decoded with
// that encoding (and the same string flows through the async iterator), exactly
// like node. Without an encoding the raw Buffer is delivered unchanged.
function decodeOutputChunk(stream, chunk) {
  const encoding = stream._readableEncoding;
  if (!encoding) {
    return chunk;
  }
  if (typeof chunk === "string") {
    return chunk;
  }
  if (typeof Buffer !== "undefined" && Buffer.isBuffer(chunk)) {
    return chunk.toString(encoding);
  }
  if (chunk instanceof Uint8Array) {
    return typeof Buffer !== "undefined" ? Buffer.from(chunk).toString(encoding) : String(chunk);
  }
  return chunk;
}
function scheduleOutputFlush(stream) {
  if (stream._flushScheduled) {
    return;
  }
  stream._flushScheduled = true;
  queueMicrotask(() => {
    stream._flushScheduled = false;
    if (stream._bufferedChunks.length > 0 && hasOutputListeners(stream, "data")) {
      const chunks = stream._bufferedChunks.splice(0, stream._bufferedChunks.length);
      for (const chunk of chunks) {
        stream.emit("data", chunk);
      }
    } else if (stream._bufferedChunks.length > 0 && hasOutputListeners(stream, "readable")) {
      stream.emit("readable");
    }
    if (stream._ended && !stream._endEmitted && stream._bufferedChunks.length === 0) {
      stream.emit("end");
    }
  });
}
function readBufferedOutputChunk(stream, size) {
  const chunk = stream._bufferedChunks.shift();
  if (chunk === void 0) {
    return null;
  }
  if (Number.isInteger(size) && size > 0 && chunk.length > size) {
    const head = typeof chunk === "string" ? chunk.slice(0, size) : chunk.subarray(0, size);
    const tail = typeof chunk === "string" ? chunk.slice(size) : chunk.subarray(size);
    stream._bufferedChunks.unshift(tail);
    return head;
  }
  if (stream._ended && stream._bufferedChunks.length === 0) scheduleOutputFlush(stream);
  return chunk;
}
function checkStreamMaxListeners(stream, event) {
  if (!(stream._maxListenersWarned instanceof Set)) {
    stream._maxListenersWarned = /* @__PURE__ */ new Set();
  }
  if (stream._maxListeners > 0 && !stream._maxListenersWarned.has(event)) {
    const total = (stream._listeners[event]?.length ?? 0) + (stream._onceListeners[event]?.length ?? 0);
    if (total > stream._maxListeners) {
      stream._maxListenersWarned.add(event);
      const warning = `MaxListenersExceededWarning: Possible EventEmitter memory leak detected. ${total} ${event} listeners added. MaxListeners is ${stream._maxListeners}. Use emitter.setMaxListeners() to increase limit`;
      if (typeof console !== "undefined" && console.error) {
        console.error(warning);
      }
    }
  }
}
function createOutputAsyncIterator(stream) {
  const queuedChunks = [];
  const queuedErrors = [];
  const pendingResolves = [];
  let finished = false;
  const settlePending = () => {
    while (pendingResolves.length > 0) {
      const resolve = pendingResolves.shift();
      if (queuedErrors.length > 0) {
        resolve(Promise.reject(queuedErrors.shift()));
        continue;
      }
      if (queuedChunks.length > 0) {
        resolve(Promise.resolve({ done: false, value: queuedChunks.shift() }));
        continue;
      }
      if (finished) {
        resolve(Promise.resolve({ done: true, value: void 0 }));
        continue;
      }
      pendingResolves.unshift(resolve);
      break;
    }
  };
  const onData = (chunk) => {
    queuedChunks.push(chunk);
    settlePending();
  };
  const onEnd = () => {
    finished = true;
    settlePending();
  };
  const onError = (error) => {
    queuedErrors.push(error);
    finished = true;
    settlePending();
  };
  stream.on("data", onData);
  stream.on("end", onEnd);
  stream.on("close", onEnd);
  stream.on("error", onError);
  scheduleOutputFlush(stream);
  return {
    next() {
      if (queuedErrors.length > 0) {
        return Promise.reject(queuedErrors.shift());
      }
      if (queuedChunks.length > 0) {
        return Promise.resolve({ done: false, value: queuedChunks.shift() });
      }
      if (finished) {
        return Promise.resolve({ done: true, value: void 0 });
      }
      return new Promise((resolve) => {
        pendingResolves.push(resolve);
      });
    },
    return() {
      stream.off("data", onData);
      stream.off("end", onEnd);
      stream.off("close", onEnd);
      stream.off("error", onError);
      finished = true;
      settlePending();
      return Promise.resolve({ done: true, value: void 0 });
    },
    [Symbol.asyncIterator]() {
      return this;
    }
  };
}
var _nextChildPid = 1e3;
var ChildProcess = class {
  _listeners = {};
  _onceListeners = {};
  _maxListeners = 10;
  _maxListenersWarned = /* @__PURE__ */ new Set();
  pid = _nextChildPid++;
  killed = false;
  exitCode = null;
  signalCode = null;
  _pendingSignalCode = null;
  connected = false;
  _sessionId = null;
  _handleId = null;
  _handleDescription = "";
  _handleRefed = false;
  _ipcEnabled = false;
  _ipcSerialization = "json";
  _ipcReady = false;
  _ipcStdoutBuffer = "";
  _ipcQueuedMessages = [];
  _ipcOutboundQueue = [];
  _ipcOutboundBytes = 0;
  spawnfile = "";
  spawnargs = [];
  stdin;
  stdout;
      stderr;
      stdio;
  constructor() {
    this.stdin = {
      writable: true,
      destroyed: false,
      _listeners: {},
      _onceListeners: {},
      write(_data, encodingOrCallback, callback) {
        const done = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
        if (done) {
          queueMicrotask(() => done(null));
        }
        return true;
      },
      end(dataOrCallback, encodingOrCallback, callback) {
        const done = typeof dataOrCallback === "function" ? dataOrCallback : typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
        this.writable = false;
        if (done) {
          queueMicrotask(() => done());
        }
      },
      destroy() {
        this.writable = false;
        this.destroyed = true;
        this.emit("close");
        return this;
      },
      on(event, listener) {
        if (!this._listeners[event]) this._listeners[event] = [];
        this._listeners[event].push(listener);
        return this;
      },
      addListener(event, listener) {
        return this.on(event, listener);
      },
      once(event, listener) {
        if (!this._onceListeners[event]) this._onceListeners[event] = [];
        this._onceListeners[event].push(listener);
        return this;
      },
      off(event, listener) {
        if (this._listeners[event]) {
          const idx = this._listeners[event].indexOf(listener);
          if (idx !== -1) this._listeners[event].splice(idx, 1);
        }
        if (this._onceListeners[event]) {
          const idx = this._onceListeners[event].indexOf(listener);
          if (idx !== -1) this._onceListeners[event].splice(idx, 1);
        }
        return this;
      },
      removeListener(event, listener) {
        return this.off(event, listener);
      },
      emit(event, ...args) {
        let handled = false;
        if (this._listeners[event]) {
          this._listeners[event].forEach((fn) => {
            fn(...args);
            handled = true;
          });
        }
        if (this._onceListeners[event]) {
          this._onceListeners[event].forEach((fn) => {
            fn(...args);
            handled = true;
          });
          this._onceListeners[event] = [];
        }
        return handled;
      }
    };
    this.stdout = {
      readable: true,
      readableEnded: false,
      isTTY: false,
      destroyed: false,
      _listeners: {},
      _onceListeners: {},
      _bufferedChunks: [],
      _ended: false,
      _endEmitted: false,
      _flushScheduled: false,
      _maxListeners: 10,
      _maxListenersWarned: /* @__PURE__ */ new Set(),
      _pipeListeners: /* @__PURE__ */ new Map(),
      on(event, listener) {
        if (!this._listeners[event]) this._listeners[event] = [];
        this._listeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "readable" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      addListener(event, listener) {
        return this.on(event, listener);
      },
      once(event, listener) {
        if (!this._onceListeners[event]) this._onceListeners[event] = [];
        this._onceListeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "readable" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      off(event, listener) {
        if (this._listeners[event]) {
          const idx = this._listeners[event].indexOf(listener);
          if (idx !== -1) this._listeners[event].splice(idx, 1);
        }
        if (this._onceListeners[event]) {
          const idx = this._onceListeners[event].indexOf(listener);
          if (idx !== -1) this._onceListeners[event].splice(idx, 1);
        }
        return this;
      },
      removeListener(event, listener) {
        return this.off(event, listener);
      },
      emit(event, ...args) {
        if (event === "data") {
          args[0] = decodeOutputChunk(this, args[0]);
          if (!hasOutputListeners(this, "data")) {
            this._bufferedChunks.push(args[0]);
            if (hasOutputListeners(this, "readable")) {
              scheduleOutputFlush(this);
            }
            return false;
          }
        }
        if (event === "end") {
          this._ended = true;
          if (this._bufferedChunks.length > 0) {
            scheduleOutputFlush(this);
            return false;
          }
          if (this._endEmitted) return false;
          this._endEmitted = true;
          this.readableEnded = true;
          this.readable = false;
          if (!hasOutputListeners(this, "end")) {
            return false;
          }
        }
        if (this._listeners[event]) {
          this._listeners[event].forEach((fn) => fn(...args));
        }
        if (this._onceListeners[event]) {
          this._onceListeners[event].forEach((fn) => fn(...args));
          this._onceListeners[event] = [];
        }
        return true;
      },
      read(size) {
        return readBufferedOutputChunk(this, size);
      },
      setEncoding(encoding) {
        this._readableEncoding = encoding == null || encoding === "buffer" ? null : String(encoding);
        return this;
      },
      setMaxListeners(n) {
        this._maxListeners = n;
        return this;
      },
      getMaxListeners() {
        return this._maxListeners;
      },
      pipe(dest) {
        if (!this._pipeListeners.has(dest)) {
          const onData = (chunk) => dest?.write?.(chunk);
          this._pipeListeners.set(dest, onData);
          this.on("data", onData);
        }
        return dest;
      },
      unpipe(dest) {
        if (dest === undefined) {
          for (const [target, listener] of this._pipeListeners) {
            this.off("data", listener);
            target?.emit?.("unpipe", this);
          }
          this._pipeListeners.clear();
          return this;
        }
        const listener = this._pipeListeners.get(dest);
        if (listener) {
          this.off("data", listener);
          this._pipeListeners.delete(dest);
          dest?.emit?.("unpipe", this);
        }
        return this;
      },
      pause() {
        return this;
      },
      resume() {
        return this;
      },
      destroy() {
        this.readable = false;
        this._ended = true;
        this.destroyed = true;
        this.emit("close");
        return this;
      },
      [Symbol.asyncIterator]() {
        return createOutputAsyncIterator(this);
      }
    };
    this.stderr = {
      readable: true,
      readableEnded: false,
      isTTY: false,
      destroyed: false,
      _listeners: {},
      _onceListeners: {},
      _bufferedChunks: [],
      _ended: false,
      _endEmitted: false,
      _flushScheduled: false,
      _maxListeners: 10,
      _maxListenersWarned: /* @__PURE__ */ new Set(),
      _pipeListeners: /* @__PURE__ */ new Map(),
      on(event, listener) {
        if (!this._listeners[event]) this._listeners[event] = [];
        this._listeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "readable" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      addListener(event, listener) {
        return this.on(event, listener);
      },
      once(event, listener) {
        if (!this._onceListeners[event]) this._onceListeners[event] = [];
        this._onceListeners[event].push(listener);
        checkStreamMaxListeners(this, event);
        if (event === "data" || event === "readable" || event === "end") {
          scheduleOutputFlush(this);
        }
        return this;
      },
      off(event, listener) {
        if (this._listeners[event]) {
          const idx = this._listeners[event].indexOf(listener);
          if (idx !== -1) this._listeners[event].splice(idx, 1);
        }
        if (this._onceListeners[event]) {
          const idx = this._onceListeners[event].indexOf(listener);
          if (idx !== -1) this._onceListeners[event].splice(idx, 1);
        }
        return this;
      },
      removeListener(event, listener) {
        return this.off(event, listener);
      },
      emit(event, ...args) {
        if (event === "data") {
          args[0] = decodeOutputChunk(this, args[0]);
          if (!hasOutputListeners(this, "data")) {
            this._bufferedChunks.push(args[0]);
            if (hasOutputListeners(this, "readable")) {
              scheduleOutputFlush(this);
            }
            return false;
          }
        }
        if (event === "end") {
          this._ended = true;
          if (this._bufferedChunks.length > 0) {
            scheduleOutputFlush(this);
            return false;
          }
          if (this._endEmitted) return false;
          this._endEmitted = true;
          this.readableEnded = true;
          this.readable = false;
          if (!hasOutputListeners(this, "end")) {
            return false;
          }
        }
        if (this._listeners[event]) {
          this._listeners[event].forEach((fn) => fn(...args));
        }
        if (this._onceListeners[event]) {
          this._onceListeners[event].forEach((fn) => fn(...args));
          this._onceListeners[event] = [];
        }
        return true;
      },
      read(size) {
        return readBufferedOutputChunk(this, size);
      },
      setEncoding(encoding) {
        this._readableEncoding = encoding == null || encoding === "buffer" ? null : String(encoding);
        return this;
      },
      setMaxListeners(n) {
        this._maxListeners = n;
        return this;
      },
      getMaxListeners() {
        return this._maxListeners;
      },
      pipe(dest) {
        if (!this._pipeListeners.has(dest)) {
          const onData = (chunk) => dest?.write?.(chunk);
          this._pipeListeners.set(dest, onData);
          this.on("data", onData);
        }
        return dest;
      },
      unpipe(dest) {
        if (dest === undefined) {
          for (const [target, listener] of this._pipeListeners) {
            this.off("data", listener);
            target?.emit?.("unpipe", this);
          }
          this._pipeListeners.clear();
          return this;
        }
        const listener = this._pipeListeners.get(dest);
        if (listener) {
          this.off("data", listener);
          this._pipeListeners.delete(dest);
          dest?.emit?.("unpipe", this);
        }
        return this;
      },
      pause() {
        return this;
      },
      resume() {
        return this;
      },
      destroy() {
        this.readable = false;
        this._ended = true;
        this.destroyed = true;
        this.emit("close");
        return this;
      },
      [Symbol.asyncIterator]() {
        return createOutputAsyncIterator(this);
      }
    };
    this.stdio = [this.stdin, this.stdout, this.stderr];
  }
  on(event, listener) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(listener);
    this._checkMaxListeners(event);
    if (event === "message") {
      this._flushQueuedIpcMessages();
    }
    return this;
  }
  addListener(event, listener) {
    return this.on(event, listener);
  }
  once(event, listener) {
    if (!this._onceListeners[event]) this._onceListeners[event] = [];
    this._onceListeners[event].push(listener);
    this._checkMaxListeners(event);
    if (event === "message") {
      this._flushQueuedIpcMessages();
    }
    return this;
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
  setMaxListeners(n) {
    this._maxListeners = n;
    return this;
  }
  getMaxListeners() {
    return this._maxListeners;
  }
  _checkMaxListeners(event) {
    if (!(this._maxListenersWarned instanceof Set)) {
      this._maxListenersWarned = /* @__PURE__ */ new Set();
    }
    if (this._maxListeners > 0 && !this._maxListenersWarned.has(event)) {
      const total = (this._listeners[event]?.length ?? 0) + (this._onceListeners[event]?.length ?? 0);
      if (total > this._maxListeners) {
        this._maxListenersWarned.add(event);
        const warning = `MaxListenersExceededWarning: Possible EventEmitter memory leak detected. ${total} ${event} listeners added to [ChildProcess]. MaxListeners is ${this._maxListeners}. Use emitter.setMaxListeners() to increase limit`;
        if (typeof console !== "undefined" && console.error) {
          console.error(warning);
        }
      }
    }
  }
  _hasIpcMessageListeners() {
    return (this._listeners.message?.length ?? 0) > 0 || (this._onceListeners.message?.length ?? 0) > 0;
  }
  _emitOrQueueIpcMessage(message) {
    if (!this._hasIpcMessageListeners()) {
      this._ipcQueuedMessages.push(message);
      return false;
    }
    return this.emit("message", message, void 0);
  }
  _flushQueuedIpcMessages() {
    if (this._ipcQueuedMessages.length === 0) {
      return;
    }
    queueMicrotask(() => {
      while (this._ipcQueuedMessages.length > 0 && this._hasIpcMessageListeners()) {
        this.emit("message", this._ipcQueuedMessages.shift(), void 0);
      }
    });
  }
  _flushIpcOutboundQueue() {
    while (this._ipcReady && this._ipcOutboundQueue.length > 0) {
      const queued = this._ipcOutboundQueue.shift();
      this._ipcOutboundBytes -= queued.frame.length;
      this.stdin.write(queued.frame, "utf8", queued.callback);
    }
  }
  emit(event, ...args) {
    let handled = false;
    if (this._listeners[event]) {
      this._listeners[event].forEach((fn) => {
        fn(...args);
        handled = true;
      });
    }
    if (this._onceListeners[event]) {
      this._onceListeners[event].forEach((fn) => {
        fn(...args);
        handled = true;
      });
      this._onceListeners[event] = [];
    }
    return handled;
  }
  kill(_signal) {
    const normalizedSignal = normalizeChildProcessSignal(_signal);
    this.killed = true;
    this._pendingSignalCode = normalizedSignal.signalCode;
    return true;
  }
  ref() {
    if (!this._handleRefed && this._handleId && typeof _registerHandle === "function") {
      _registerHandle(this._handleId, this._handleDescription);
      this._handleRefed = true;
    }
    return this;
  }
  unref() {
    if (this._handleRefed && this._handleId && typeof _unregisterHandle === "function") {
      _unregisterHandle(this._handleId);
      this._handleRefed = false;
    }
    return this;
  }
  disconnect() {
    this.connected = false;
    this.emit("disconnect");
  }
  send(message, sendHandleOrOptions, optionsOrCallback, maybeCallback) {
    if (!this.connected || !this._ipcEnabled || this._sessionId == null) {
      return false;
    }
    const callback = typeof sendHandleOrOptions === "function" ? sendHandleOrOptions : typeof optionsOrCallback === "function" ? optionsOrCallback : maybeCallback;
    try {
      const frame = encodeChildProcessIpcFrame(message, this._ipcSerialization);
      if (!this._ipcReady) {
        if (this._ipcOutboundQueue.length >= 1024 || this._ipcOutboundBytes + frame.length > 8 * 1024 * 1024) {
          const error = new Error("ERR_RESOURCE_BUDGET_EXCEEDED: pre-ready child_process IPC queue exceeds 1024 messages or 8388608 bytes; wait for the child spawn/IPC channel before sending more data");
          error.code = "ERR_RESOURCE_BUDGET_EXCEEDED";
          if (callback) queueMicrotask(() => callback(error));
          else queueMicrotask(() => this.emit("error", error));
          return false;
        }
        this._ipcOutboundQueue.push({ frame, callback });
        this._ipcOutboundBytes += frame.length;
        return true;
      }
      this.stdin.write(frame, "utf8", callback);
      return true;
    } catch (error) {
      if (callback) {
        queueMicrotask(() => callback(error));
        return false;
      }
      this.emit("error", error);
      return false;
    }
  }
  _complete(stdout, stderr, code) {
    const signalCode = this.signalCode;
    this._pendingSignalCode = null;
    this.signalCode = signalCode ?? null;
    this.exitCode = signalCode == null ? code : null;
    if (stdout) {
      const buf = typeof Buffer !== "undefined" ? Buffer.from(stdout) : stdout;
      this.stdout.emit("data", buf);
    }
    if (stderr) {
      const buf = typeof Buffer !== "undefined" ? Buffer.from(stderr) : stderr;
      this.stderr.emit("data", buf);
    }
    this.emit("exit", this.exitCode, this.signalCode);
    this.stdout.emit("end");
    this.stderr.emit("end");
    queueMicrotask(() => this.emit("close", this.exitCode, this.signalCode));
  }
};
function exec(command, options, callback) {
  if (typeof options === "function") {
    callback = options;
    options = {};
  }
  const child = spawn(command, [], {
    ...options,
    shell: true
  });
  child.spawnargs = [command];
  child.spawnfile = command;
  const maxBuffer = options?.maxBuffer ?? 1024 * 1024;
  let stdout = "";
  let stderr = "";
  let stdoutBytes = 0;
  let stderrBytes = 0;
  let maxBufferExceeded = false;
  let callbackSettled = false;
  let spawnError = null;
  const finishExec = (error) => {
    if (!callback || callbackSettled) {
      return;
    }
    callbackSettled = true;
    callback(error, stdout, stderr);
  };
  child.stdout.on("data", (data) => {
    if (maxBufferExceeded) return;
    const chunk = String(data);
    stdout += chunk;
    stdoutBytes += chunk.length;
    if (stdoutBytes > maxBuffer) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });
  child.stderr.on("data", (data) => {
    if (maxBufferExceeded) return;
    const chunk = String(data);
    stderr += chunk;
    stderrBytes += chunk.length;
    if (stderrBytes > maxBuffer) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });
  child.on("close", (...args) => {
    const code = args[0];
    if (callback) {
      if (maxBufferExceeded) {
        const err = new Error("stdout maxBuffer length exceeded");
        err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
        err.killed = true;
        err.cmd = command;
        err.stdout = stdout;
        err.stderr = stderr;
        finishExec(err);
      } else if (code !== 0 && spawnError == null) {
        const err = new Error("Command failed: " + command);
        err.code = code;
        err.killed = false;
        err.signal = null;
        err.cmd = command;
        err.stdout = stdout;
        err.stderr = stderr;
        finishExec(err);
      } else {
        finishExec(null);
      }
    }
  });
  child.on("error", (err) => {
    if (callback) {
      const error = err instanceof Error ? err : new Error(String(err));
      spawnError = error;
      error.cmd = command;
      error.stdout = stdout;
      error.stderr = stderr;
      finishExec(error);
    }
  });
  return child;
}
function execSync(command, options) {
  const opts = options || {};
  if (typeof _childProcessSpawnSync === "undefined") {
    throw new Error("child_process.execSync requires CommandExecutor to be configured");
  }
  const effectiveCwd = opts.cwd ?? (typeof process !== "undefined" ? process.cwd() : "/");
  const maxBuffer = opts.maxBuffer ?? 1024 * 1024;
  const jsonResult = _childProcessSpawnSync.applySyncPromise(void 0, [
    command,
    JSON.stringify([]),
    JSON.stringify({
      cwd: effectiveCwd,
      env: opts.env,
      argv0: opts.argv0 == null ? void 0 : String(opts.argv0),
      input: opts.input == null ? null : encodeBridgeBytes(opts.input),
      maxBuffer,
      shell: true,
      timeout: Number.isInteger(opts.timeout) && opts.timeout > 0 ? opts.timeout : null,
      killSignal: normalizeChildProcessSignal(opts.killSignal).signalCode ?? "SIGTERM"
    })
  ]);
  const result = typeof jsonResult === "string" ? JSON.parse(jsonResult) : jsonResult;
  const execSyncStdio = Array.isArray(opts.stdio) ? opts.stdio : opts.stdio === "inherit" ? ["inherit", "inherit", "inherit"] : [];
  // Node fd inheritance for the sync path: the captured stdout/stderr is written
  // to the inherited descriptor and removed from the returned value, matching
  // native node where the redirected stream does not also come back as output.
  if (redirectSyncOutputToInheritedFd(execSyncStdio[1], result.stdout)) {
    result.stdout = typeof result.stdout === "string" ? "" : Buffer.from("");
  }
  redirectSyncOutputToInheritedFd(execSyncStdio[2], result.stderr);
  if (result.timedOut) {
    const err = new Error(`spawnSync ${command} ETIMEDOUT`);
    err.code = "ETIMEDOUT";
    err.status = result.signal == null && typeof result.code === "number" ? result.code : null;
    err.signal = result.signal ?? null;
    err.stdout = result.stdout;
    err.stderr = result.stderr;
    err.output = [null, result.stdout, result.stderr];
    throw err;
  }
  if (result.maxBufferExceeded) {
    const err = new Error("stdout maxBuffer length exceeded");
    err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
    err.stdout = result.stdout;
    err.stderr = result.stderr;
    throw err;
  }
  if (result.code !== 0 || result.signal != null) {
    const err = new Error("Command failed: " + command);
    err.status = result.signal == null ? result.code : null;
    err.signal = result.signal ?? null;
    err.stdout = result.stdout;
    err.stderr = result.stderr;
    err.output = [null, result.stdout, result.stderr];
    throw err;
  }
  if (opts.encoding === "buffer" || !opts.encoding) {
    return typeof Buffer !== "undefined" ? Buffer.from(result.stdout) : result.stdout;
  }
  return result.stdout;
}
function spawn(command, args, options) {
  let argsArray = [];
  let opts = {};
  if (!Array.isArray(args)) {
    opts = args || {};
  } else {
    argsArray = args;
    opts = options || {};
  }
  const child = new ChildProcess();
  if (opts.__agentOSForkIpc === true) {
    child._ipcEnabled = true;
    child.connected = true;
  }
  child.spawnfile = command;
  child.spawnargs = [command, ...argsArray];
  child.detached = opts.detached === true;
  const stdio = Array.isArray(opts.stdio) ? opts.stdio : opts.stdio === "inherit" ? ["inherit", "inherit", "inherit"] : [];
  // Node fd inheritance: when stdio[1]/stdio[2] is a numeric fd the child's
  // stdout/stderr is wired to that (host/VFS) descriptor, so the bytes are
  // written there instead of being delivered on child.stdout/child.stderr
  // (which native node leaves null in that mode).
  child._stdoutFd = typeof stdio[1] === "number" ? stdio[1] : null;
  child._stderrFd = typeof stdio[2] === "number" ? stdio[2] : null;
  child._stdoutPath = childInheritedFdPath(child._stdoutFd);
  child._stderrPath = childInheritedFdPath(child._stderrFd);
  child._inheritedFds = [];
  for (const fd of [child._stdoutFd, child._stderrFd]) {
    if (typeof fd === "number") {
      retainChildInheritedFd(fd);
      child._inheritedFds.push(fd);
    }
  }
  if (typeof _childProcessSpawnStart !== "undefined") {
    let spawnResult;
    try {
      const effectiveCwd = opts.cwd ?? (typeof process !== "undefined" ? process.cwd() : "/");
      spawnResult = normalizeChildProcessBridgePayload(_childProcessSpawnStart.applySync(void 0, [
        command,
        JSON.stringify(argsArray),
        JSON.stringify({
          cwd: effectiveCwd,
          env: opts.env,
          argv0: opts.argv0 == null ? void 0 : String(opts.argv0),
          shell: opts.shell === true || typeof opts.shell === "string",
          detached: opts.detached === true,
          pty: opts.agentosPty && typeof opts.agentosPty === "object" ? {
            cols: Number.isInteger(opts.agentosPty.cols) && opts.agentosPty.cols > 0 ? opts.agentosPty.cols : 80,
            rows: Number.isInteger(opts.agentosPty.rows) && opts.agentosPty.rows > 0 ? opts.agentosPty.rows : 24
          } : opts.agentosPty === true ? { cols: 80, rows: 24 } : null
        })
      ]));
    } catch (error) {
      const spawnError = error instanceof Error ? error : new Error(String(error));
      if (spawnError.code == null && /command not found:/i.test(String(spawnError.message || ""))) {
        spawnError.code = "ENOENT";
      } else if (
        spawnError.code == null &&
        /ERR_NATIVE_BINARY_NOT_SUPPORTED\b/i.test(String(spawnError.message || ""))
      ) {
        spawnError.code = "ERR_NATIVE_BINARY_NOT_SUPPORTED";
      }
      queueMicrotask(() => {
        child.emit("error", spawnError);
      });
      return child;
    }
    const sessionId = typeof spawnResult === "object" && spawnResult !== null ? spawnResult.childId : spawnResult;
    childProcessInstances.set(sessionId, child);
    child._sessionId = sessionId;
    if (typeof _registerHandle === "function") {
      child._handleId = `child:${sessionId}`;
      child._handleDescription = `child_process: ${command} ${argsArray.join(" ")}`;
      _registerHandle(child._handleId, child._handleDescription);
      child._handleRefed = true;
    }
    queueMicrotask(() => {
      const events = earlyChildProcessEvents.get(sessionId);
      if (!events) return;
      earlyChildProcessEvents.delete(sessionId);
      for (const event of events) {
        routeChildProcessEvent(sessionId, event.type, event.data);
      }
    });
    child.stdin.write = (data, encodingOrCallback, callback) => {
      const done = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      if (!child.stdin.writable || child.stdin.destroyed) {
        const error = new Error("Cannot call write after a stream was destroyed");
        error.code = "ERR_STREAM_DESTROYED";
        queueMicrotask(() => {
          if (done) done(error);
          else child.stdin.emit("error", error);
        });
        return false;
      }
      if (typeof _childProcessStdinWrite === "undefined") return false;
      const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
      try {
        _childProcessStdinWrite.applySync(void 0, [sessionId, bytes]);
      } catch (error) {
        if (done) {
          queueMicrotask(() => done(error));
          return false;
        }
        child.stdin.emit("error", error);
        return false;
      }
      if (done) {
        queueMicrotask(() => done(null));
      }
      return true;
    };
    child.stdin.end = (dataOrCallback, encodingOrCallback, callback) => {
      const done = typeof dataOrCallback === "function" ? dataOrCallback : typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      if (dataOrCallback != null && typeof dataOrCallback !== "function") {
        child.stdin.write(dataOrCallback, typeof encodingOrCallback === "string" ? encodingOrCallback : void 0);
      }
      if (typeof _childProcessStdinClose !== "undefined") {
        try {
          _childProcessStdinClose.applySync(void 0, [sessionId]);
        } catch (error) {
          if (done) {
            queueMicrotask(() => done(error));
            return;
          }
          child.stdin.emit("error", error);
          return;
        }
      }
      child.stdin.writable = false;
      if (done) {
        queueMicrotask(() => done());
      }
    };
    child.stdin.destroy = () => {
      child.stdin.end();
      child.stdin.destroyed = true;
      child.stdin.emit("close");
      return child.stdin;
    };
    child.kill = (signal) => {
      if (typeof _childProcessKill === "undefined") return false;
      const normalizedSignal = normalizeChildProcessSignal(signal);
      _childProcessKill.applySync(void 0, [sessionId, normalizedSignal.bridgeSignal]);
      child.killed = true;
      child._pendingSignalCode = normalizedSignal.signalCode;
      return true;
    };
    child.resizePty = (cols, rows) => {
      if (typeof _childProcessPtyResize === "undefined") {
        throw new Error("child_process PTY resize bridge is unavailable");
      }
      _childProcessPtyResize.applySync(void 0, [sessionId, cols, rows]);
      return child;
    };
    child.pid = typeof spawnResult === "object" && spawnResult !== null ? Number(spawnResult.pid) || -1 : Number(sessionId) || -1;
    if (stdio[1] === "inherit" || stdio[1] === 1) {
      child.stdout.on("data", (chunk) => process.stdout.write(chunk));
    }
    if (stdio[2] === "inherit" || stdio[2] === 2) {
      child.stderr.on("data", (chunk) => process.stderr.write(chunk));
    }
    setTimeout(() => child.emit("spawn"), 0);
    return child;
  }
  const err = new Error(
    "child_process.spawn requires CommandExecutor to be configured"
  );
  setTimeout(() => {
    child.emit("error", err);
    child._complete("", err.message, 1);
  }, 0);
  return child;
}
function spawnSync(command, args, options) {
  let argsArray = [];
  let opts = {};
  if (!Array.isArray(args)) {
    opts = args || {};
  } else {
    argsArray = args;
    opts = options || {};
  }
  if (typeof _childProcessSpawnSync === "undefined") {
    return {
      pid: _nextChildPid++,
      output: [null, "", "child_process.spawnSync requires CommandExecutor to be configured"],
      stdout: "",
      stderr: "child_process.spawnSync requires CommandExecutor to be configured",
      status: 1,
      signal: null,
      error: new Error("child_process.spawnSync requires CommandExecutor to be configured")
    };
  }
  try {
    const effectiveCwd = opts.cwd ?? (typeof process !== "undefined" ? process.cwd() : "/");
    const maxBuffer = opts.maxBuffer;
    const useBufferOutput = opts.encoding == null || opts.encoding === "buffer";
    const timeout = Number.isInteger(opts.timeout) && opts.timeout > 0 ? opts.timeout : null;
    const killSignal = normalizeChildProcessSignal(opts.killSignal).signalCode ?? "SIGTERM";
    const jsonResult = _childProcessSpawnSync.applySyncPromise(void 0, [
      command,
      JSON.stringify(argsArray),
      JSON.stringify({
        cwd: effectiveCwd,
        env: opts.env,
        argv0: opts.argv0 == null ? void 0 : String(opts.argv0),
        input: opts.input == null ? null : encodeBridgeBytes(opts.input),
        maxBuffer,
        shell: opts.shell === true || typeof opts.shell === "string",
        timeout,
        killSignal
      })
    ]);
    const result = typeof jsonResult === "string" ? JSON.parse(jsonResult) : jsonResult;
    const spawnSyncStdio = Array.isArray(opts.stdio) ? opts.stdio : opts.stdio === "inherit" ? ["inherit", "inherit", "inherit"] : [];
    let stdoutValue = useBufferOutput && typeof Buffer !== "undefined" ? Buffer.from(result.stdout) : result.stdout;
    let stderrValue = useBufferOutput && typeof Buffer !== "undefined" ? Buffer.from(result.stderr) : result.stderr;
    // Node fd inheritance: redirect captured output to the inherited descriptor
    // and null it out of the returned result, like native node.
    if (redirectSyncOutputToInheritedFd(spawnSyncStdio[1], stdoutValue)) {
      stdoutValue = useBufferOutput && typeof Buffer !== "undefined" ? Buffer.from("") : "";
    }
    if (redirectSyncOutputToInheritedFd(spawnSyncStdio[2], stderrValue)) {
      stderrValue = useBufferOutput && typeof Buffer !== "undefined" ? Buffer.from("") : "";
    }
    if (result.timedOut) {
      const err = new Error(`spawnSync ${command} ETIMEDOUT`);
      err.code = "ETIMEDOUT";
      return {
        pid: _nextChildPid++,
        output: [null, stdoutValue, stderrValue],
        stdout: stdoutValue,
        stderr: stderrValue,
        status: typeof result.code === "number" && result.signal == null ? result.code : null,
        signal: result.signal ?? null,
        error: err
      };
    }
    if (result.maxBufferExceeded) {
      const err = new Error("stdout maxBuffer length exceeded");
      err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
      return {
        pid: _nextChildPid++,
        output: [null, stdoutValue, stderrValue],
        stdout: stdoutValue,
        stderr: stderrValue,
        status: typeof result.code === "number" && result.signal == null ? result.code : null,
        signal: result.signal ?? null,
        error: err
      };
    }
    return {
      pid: _nextChildPid++,
      output: [null, stdoutValue, stderrValue],
      stdout: stdoutValue,
      stderr: stderrValue,
      status: typeof result.code === "number" && result.signal == null ? result.code : null,
      signal: result.signal ?? null,
      error: void 0
    };
  } catch (err) {
    if (
      err &&
      typeof err === "object" &&
      err.code == null &&
      /ERR_NATIVE_BINARY_NOT_SUPPORTED\b/i.test(String(err.message || err))
    ) {
      err.code = "ERR_NATIVE_BINARY_NOT_SUPPORTED";
    }
    const errMsg = err instanceof Error ? err.message : String(err);
    const useBufferOutput = opts.encoding == null || opts.encoding === "buffer";
    const stdoutValue = useBufferOutput && typeof Buffer !== "undefined" ? Buffer.from("") : "";
    const stderrValue = useBufferOutput && typeof Buffer !== "undefined" ? Buffer.from(errMsg) : errMsg;
    return {
      pid: _nextChildPid++,
      output: [null, stdoutValue, stderrValue],
      stdout: stdoutValue,
      stderr: stderrValue,
      status: 1,
      signal: null,
      error: err instanceof Error ? err : new Error(String(err))
    };
  }
}
function execFile(file, args, options, callback) {
  let argsArray = [];
  let opts = {};
  let cb;
  if (typeof args === "function") {
    cb = args;
  } else if (typeof options === "function") {
    argsArray = args.slice();
    cb = options;
  } else {
    argsArray = Array.isArray(args) ? args : [];
    opts = options || {};
    cb = callback;
  }
  const maxBuffer = opts.maxBuffer ?? 1024 * 1024;
  const child = spawn(file, argsArray, opts);
  let stdout = "";
  let stderr = "";
  let stdoutBytes = 0;
  let stderrBytes = 0;
  let maxBufferExceeded = false;
  child.stdout.on("data", (data) => {
    const chunk = String(data);
    stdout += chunk;
    stdoutBytes += chunk.length;
    if (stdoutBytes > maxBuffer && !maxBufferExceeded) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });
  child.stderr.on("data", (data) => {
    const chunk = String(data);
    stderr += chunk;
    stderrBytes += chunk.length;
    if (stderrBytes > maxBuffer && !maxBufferExceeded) {
      maxBufferExceeded = true;
      child.kill("SIGTERM");
    }
  });
  child.on("close", (...args2) => {
    const code = args2[0];
    if (cb) {
      if (maxBufferExceeded) {
        const err = new Error("stdout maxBuffer length exceeded");
        err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
        err.killed = true;
        err.stdout = stdout;
        err.stderr = stderr;
        cb(err, stdout, stderr);
      } else if (code !== 0) {
        const err = new Error("Command failed: " + file);
        err.code = code;
        err.stdout = stdout;
        err.stderr = stderr;
        cb(err, stdout, stderr);
      } else {
        cb(null, stdout, stderr);
      }
    }
  });
  child.on("error", (err) => {
    if (cb) {
      cb(err, stdout, stderr);
    }
  });
  return child;
}
Object.defineProperty(execFile, Symbol.for("nodejs.util.promisify.custom"), {
  configurable: true,
  value(file, args, options) {
    return new Promise((resolve, reject) => {
      execFile(file, args, options, (error, stdout, stderr) => {
        if (error) {
          reject(error);
          return;
        }
        resolve({ stdout, stderr });
      });
    });
  },
});
function execFileSync(file, args, options) {
  let argsArray = [];
  let opts = {};
  if (!Array.isArray(args)) {
    opts = args || {};
  } else {
    argsArray = args;
    opts = options || {};
  }
  const maxBuffer = opts.maxBuffer ?? 1024 * 1024;
  const result = spawnSync(file, argsArray, { ...opts, maxBuffer });
  if (result.error && String(result.error.code) === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER") {
    throw result.error;
  }
  if (result.status !== 0) {
    const err = new Error("Command failed: " + file);
    err.status = result.status ?? void 0;
    err.stdout = String(result.stdout);
    err.stderr = String(result.stderr);
    throw err;
  }
  if (opts.encoding === "buffer" || !opts.encoding) {
    return result.stdout;
  }
  return typeof result.stdout === "string" ? result.stdout : result.stdout.toString(opts.encoding);
}
function fork(modulePath, args, options) {
  if (typeof modulePath !== "string" || modulePath.length === 0) {
    throw new TypeError("The \"modulePath\" argument must be of type string");
  }
  let argsArray = [];
  let opts = {};
  if (Array.isArray(args)) {
    argsArray = args.slice();
    opts = options || {};
  } else {
    opts = args || {};
  }
  const effectiveCwd = opts.cwd ?? (typeof process !== "undefined" ? process.cwd() : "/");
  const execArgv = Array.isArray(opts.execArgv) ? opts.execArgv : typeof process !== "undefined" && Array.isArray(process.execArgv) ? process.execArgv : [];
  const preloadModules = [];
  for (let index = 0; index < execArgv.length; index += 1) {
    const argument = String(execArgv[index]);
    if (argument === "--require" || argument === "-r") {
      if (index + 1 < execArgv.length) preloadModules.push(String(execArgv[++index]));
    } else if (argument.startsWith("--require=")) {
      preloadModules.push(argument.slice("--require=".length));
    }
  }
  const env = {
    ...(typeof process !== "undefined" ? process.env : {}),
    ...(opts.env || {}),
    AGENTOS_NODE_IPC: "1",
    AGENTOS_NODE_IPC_SERIALIZATION: opts.serialization === "advanced" ? "advanced" : "json",
    AGENTOS_NODE_EXEC_ARGV: JSON.stringify(execArgv.map(String)),
    AGENTOS_NODE_PRELOAD_MODULES: JSON.stringify(preloadModules)
  };
  const child = spawn(opts.execPath || (typeof process !== "undefined" ? process.execPath : "node"), [
    modulePath,
    ...argsArray
  ], {
    ...opts,
    __agentOSForkIpc: true,
    cwd: effectiveCwd,
    env,
    shell: false
  });
  child._ipcEnabled = true;
  child._ipcSerialization = opts.serialization === "advanced" ? "advanced" : "json";
  child.connected = true;
  return child;
}
var childProcess = {
  ChildProcess,
  exec,
  execSync,
  spawn,
  spawnSync,
  execFile,
  execFileSync,
  fork
};
exposeCustomGlobal("_childProcessModule", childProcess);
var child_process_default = childProcess;
export { child_process_exports, childProcessInstances, _childInheritedFds, retainChildInheritedFd, deferCloseIfChildInheritedFd, releaseChildInheritedFd, normalizeChildProcessSessionId, normalizeChildProcessBridgePayload, CHILD_PROCESS_IPC_FRAME_PREFIX, encodeChildProcessIpcFrame, decodeChildProcessIpcFramePayload, splitChildProcessIpcFrames, writeChildOutputToInheritedFd, redirectSyncOutputToInheritedFd, routeChildProcessEvent, childProcessDispatch, hasOutputListeners, decodeOutputChunk, scheduleOutputFlush, checkStreamMaxListeners, createOutputAsyncIterator, _nextChildPid, ChildProcess, exec, execSync, spawn, spawnSync, execFile, execFileSync, fork, childProcess, child_process_default };
