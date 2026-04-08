import { createResolutionCache } from "./package-bundler.js";
import { getConsoleSetupCode } from "@secure-exec/core/internal/shared/console-formatter";
import { getRequireSetupCode } from "@secure-exec/core/internal/shared/require-setup";
import { getIsolateRuntimeSource, getInitialBridgeGlobalsSetupCode } from "@secure-exec/core";
import {
	createCommandExecutorStub,
	createFsStub,
	createNetworkStub,
	filterEnv,
	wrapCommandExecutor,
	wrapFileSystem,
	wrapNetworkAdapter,
} from "@secure-exec/core/internal/shared/permissions";
import type { NetworkAdapter, RuntimeDriver } from "@secure-exec/core";
import type {
	StdioHook,
	ExecOptions,
	ExecResult,
	RunResult,
	TimingMitigation,
} from "@secure-exec/core/internal/shared/api-types";
import type { V8ExecutionOptions, V8Runtime, V8Session, V8SessionOptions } from "@secure-exec/v8";
import { createV8Runtime } from "@secure-exec/v8";
import { getRawBridgeCode, getBridgeAttachCode } from "./bridge-loader.js";
import {
	type NodeExecutionDriverOptions,
	createBudgetState,
	clearActiveHostTimers,
	killActiveChildProcesses,
	normalizePayloadLimit,
	getExecutionTimeoutMs,
	getTimingMitigation,
	PAYLOAD_LIMIT_ERROR_CODE,
	DEFAULT_BRIDGE_BASE64_TRANSFER_BYTES,
	DEFAULT_ISOLATE_JSON_PAYLOAD_BYTES,
	DEFAULT_MAX_TIMERS,
	DEFAULT_MAX_HANDLES,
	DEFAULT_SANDBOX_CWD,
	DEFAULT_SANDBOX_HOME,
	DEFAULT_SANDBOX_TMPDIR,
} from "./isolate-bootstrap.js";
import { transformSourceForRequireSync } from "./module-source.js";
import { shouldRunAsESM } from "./module-resolver.js";
import {
	TIMEOUT_ERROR_MESSAGE,
	TIMEOUT_EXIT_CODE,
	ProcessTable,
	SocketTable,
	TimerTable,
} from "@secure-exec/core";
import {
	type BridgeHandlers,
	buildCryptoBridgeHandlers,
	buildConsoleBridgeHandlers,
	buildKernelHandleDispatchHandlers,
	buildKernelStdinDispatchHandlers,
	buildKernelTimerDispatchHandlers,
	buildModuleLoadingBridgeHandlers,
	buildMimeBridgeHandlers,
	buildTimerBridgeHandlers,
	buildFsBridgeHandlers,
	buildKernelFdBridgeHandlers,
	buildChildProcessBridgeHandlers,
	buildNetworkBridgeHandlers,
	buildNetworkSocketBridgeHandlers,
	buildModuleResolutionBridgeHandlers,
	buildPtyBridgeHandlers,
	createProcessConfigForExecution,
	resolveHttpServerResponse,
} from "./bridge-handlers.js";
import type {
	Permissions,
	VirtualFileSystem,
} from "@secure-exec/core";
import type {
	CommandExecutor,
	SpawnedProcess,
} from "@secure-exec/core";
import type { ResolutionCache } from "./package-bundler.js";
import type {
	OSConfig,
	ProcessConfig,
} from "@secure-exec/core/internal/shared/api-types";
import type { BudgetState } from "./isolate-bootstrap.js";
import { type FlattenedBinding, flattenBindingTree, BINDING_PREFIX } from "./bindings.js";
import { createNodeHostNetworkAdapter } from "./host-network-adapter.js";

export { NodeExecutionDriverOptions };

const MAX_ERROR_MESSAGE_CHARS = 8192;

type LoopbackAwareNetworkAdapter = NetworkAdapter & {
	__setLoopbackPortChecker?: (checker: (hostname: string, port: number) => boolean) => void;
};

function boundErrorMessage(message: string): string {
	if (message.length <= MAX_ERROR_MESSAGE_CHARS) return message;
	return `${message.slice(0, MAX_ERROR_MESSAGE_CHARS)}...[Truncated]`;
}

function createBridgeDriverProcess(): import("@secure-exec/core").DriverProcess {
	return {
		writeStdin() {},
		closeStdin() {},
		kill() {},
		wait: async () => 0,
		onStdout: null,
		onStderr: null,
		onExit: null,
	};
}

/** Internal state for the execution driver. */
interface DriverState {
	filesystem: VirtualFileSystem;
	commandExecutor: CommandExecutor;
	networkAdapter: NetworkAdapter;
	permissions?: Permissions;
	processConfig: ProcessConfig;
	osConfig: OSConfig;
	onStdio?: StdioHook;
	cpuTimeLimitMs?: number;
	timingMitigation: TimingMitigation;
	bridgeBase64TransferLimitBytes: number;
	isolateJsonPayloadLimitBytes: number;
	maxOutputBytes?: number;
	maxBridgeCalls?: number;
	maxTimers?: number;
	maxChildProcesses?: number;
	maxHandles?: number;
	budgetState: BudgetState;
	activeHttpServerIds: Set<number>;
	activeHttpServerClosers: Map<number, () => Promise<void>>;
	pendingHttpServerStarts: { count: number };
	activeHttpClientRequests: { count: number };
	activeChildProcesses: Map<number, SpawnedProcess>;
	activeHostTimers: Set<ReturnType<typeof setTimeout>>;
	moduleFormatCache: Map<string, "esm" | "cjs" | "json">;
	packageTypeCache: Map<string, "module" | "commonjs" | null>;
	resolutionCache: ResolutionCache;
	onPtySetRawMode?: (mode: boolean) => void;
	liveStdinSource?: NodeExecutionDriverOptions["liveStdinSource"];
}

// Shared V8 runtime process — one per Node.js process, lazy-initialized
let sharedV8Runtime: V8Runtime | null = null;
let sharedV8RuntimePromise: Promise<V8Runtime> | null = null;
let sharedV8RuntimeUsers = 0;

async function getSharedV8Runtime(): Promise<V8Runtime> {
	if (sharedV8Runtime?.isAlive) return sharedV8Runtime;
	if (sharedV8RuntimePromise) return sharedV8RuntimePromise;

	// Build bridge code for snapshot warmup
	const bridgeCode = buildFullBridgeCode();

	sharedV8RuntimePromise = createV8Runtime({
		warmupBridgeCode: bridgeCode,
	}).then((rt) => {
		sharedV8Runtime = rt;
		sharedV8RuntimePromise = null;
		return rt;
	});
	return sharedV8RuntimePromise;
}

async function releaseSharedV8Runtime(): Promise<void> {
	if (sharedV8RuntimeUsers > 0) {
		sharedV8RuntimeUsers -= 1;
	}
	if (sharedV8RuntimeUsers !== 0) {
		return;
	}

	if (sharedV8RuntimePromise) {
		void sharedV8RuntimePromise
			.then(async (runtime) => {
				if (sharedV8RuntimeUsers !== 0 || sharedV8Runtime !== runtime) {
					return;
				}
				sharedV8Runtime = null;
				await runtime.dispose().catch(() => {});
			})
			.catch(() => {});
		return;
	}

	if (!sharedV8Runtime) {
		return;
	}

	const runtime = sharedV8Runtime;
	sharedV8Runtime = null;
	await runtime.dispose().catch(() => {});
}

// Minimal polyfills for APIs the bridge IIFE expects but the Rust V8 runtime doesn't provide.
const REGEXP_COMPAT_POLYFILL = String.raw`
if (typeof globalThis.RegExp === 'function' && !globalThis.RegExp.__secureExecRgiEmojiCompat) {
  const NativeRegExp = globalThis.RegExp;
  const RGI_EMOJI_PATTERN = '^\\p{RGI_Emoji}$';
  const RGI_EMOJI_BASE_CLASS = '[\\u{00A9}\\u{00AE}\\u{203C}\\u{2049}\\u{2122}\\u{2139}\\u{2194}-\\u{21AA}\\u{231A}-\\u{23FF}\\u{24C2}\\u{25AA}-\\u{27BF}\\u{2934}-\\u{2935}\\u{2B05}-\\u{2B55}\\u{3030}\\u{303D}\\u{3297}\\u{3299}\\u{1F000}-\\u{1FAFF}]';
  const RGI_EMOJI_KEYCAP = '[#*0-9]\\uFE0F?\\u20E3';
  const RGI_EMOJI_FALLBACK_SOURCE =
    '^(?:' +
    RGI_EMOJI_KEYCAP +
    '|\\p{Regional_Indicator}{2}|' +
    RGI_EMOJI_BASE_CLASS +
    '(?:\\uFE0F|\\u200D(?:' +
    RGI_EMOJI_KEYCAP +
    '|' +
    RGI_EMOJI_BASE_CLASS +
    ')|[\\u{1F3FB}-\\u{1F3FF}])*)$';
  try {
    new NativeRegExp(RGI_EMOJI_PATTERN, 'v');
  } catch (error) {
    if (String(error && error.message || error).includes('RGI_Emoji')) {
      function CompatRegExp(pattern, flags) {
        const normalizedPattern =
          pattern instanceof NativeRegExp && flags === undefined
            ? pattern.source
            : String(pattern);
        const normalizedFlags =
          flags === undefined
            ? (pattern instanceof NativeRegExp ? pattern.flags : '')
            : String(flags);
        try {
          return new NativeRegExp(pattern, flags);
        } catch (innerError) {
          if (normalizedPattern === RGI_EMOJI_PATTERN && normalizedFlags === 'v') {
            return new NativeRegExp(RGI_EMOJI_FALLBACK_SOURCE, 'u');
          }
          throw innerError;
        }
      }
      Object.setPrototypeOf(CompatRegExp, NativeRegExp);
      CompatRegExp.prototype = NativeRegExp.prototype;
      Object.defineProperty(CompatRegExp.prototype, 'constructor', {
        value: CompatRegExp,
        writable: true,
        configurable: true,
      });
      CompatRegExp.__secureExecRgiEmojiCompat = true;
      globalThis.RegExp = CompatRegExp;
    }
  }
}
`;

const V8_POLYFILLS = `
if (typeof global === 'undefined') {
  globalThis.global = globalThis;
}
${REGEXP_COMPAT_POLYFILL}
if (typeof SharedArrayBuffer === 'undefined') {
  globalThis.SharedArrayBuffer = class SharedArrayBuffer extends ArrayBuffer {};
  var _abBL = Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'byteLength');
  if (_abBL) Object.defineProperty(SharedArrayBuffer.prototype, 'byteLength', _abBL);
  Object.defineProperty(SharedArrayBuffer.prototype, 'growable', { get() { return false; } });
}
if (!Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'resizable')) {
  Object.defineProperty(ArrayBuffer.prototype, 'resizable', { get() { return false; } });
}
if (typeof queueMicrotask === 'undefined') globalThis.queueMicrotask = (fn) => Promise.resolve().then(fn);
if (typeof atob === 'undefined') {
  globalThis.atob = (s) => {
    const b = typeof Buffer !== 'undefined' ? Buffer : null;
    if (b) return b.from(s, 'base64').toString('binary');
    // Fallback: manual base64 decode
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=';
    let out = ''; for (let i = 0; i < s.length;) {
      const a = chars.indexOf(s[i++]), b2 = chars.indexOf(s[i++]), c = chars.indexOf(s[i++]), d = chars.indexOf(s[i++]);
      out += String.fromCharCode((a<<2)|(b2>>4)); if (c!==64) out += String.fromCharCode(((b2&15)<<4)|(c>>2)); if (d!==64) out += String.fromCharCode(((c&3)<<6)|d);
    } return out;
  };
  globalThis.btoa = (s) => {
    const b = typeof Buffer !== 'undefined' ? Buffer : null;
    if (b) return b.from(s, 'binary').toString('base64');
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    let out = ''; for (let i = 0; i < s.length;) {
      const a = s.charCodeAt(i++), b2 = s.charCodeAt(i++), c = s.charCodeAt(i++);
      out += chars[a>>2] + chars[((a&3)<<4)|(b2>>4)] + (isNaN(b2) ? '=' : chars[((b2&15)<<2)|(c>>4)]) + (isNaN(c) ? '=' : chars[c&63]);
    } return out;
  };
}
if (typeof TextEncoder === 'undefined') {
  const _encodeUtf8 = (str = '') => {
    const bytes = [];
    for (let i = 0; i < str.length; i++) {
      const codeUnit = str.charCodeAt(i);
      let codePoint = codeUnit;
      if (codeUnit >= 0xD800 && codeUnit <= 0xDBFF) {
        const next = i + 1 < str.length ? str.charCodeAt(i + 1) : 0;
        if (next >= 0xDC00 && next <= 0xDFFF) {
          codePoint = 0x10000 + ((codeUnit - 0xD800) << 10) + (next - 0xDC00);
          i++;
        } else {
          codePoint = 0xFFFD;
        }
      } else if (codeUnit >= 0xDC00 && codeUnit <= 0xDFFF) {
        codePoint = 0xFFFD;
      }
      if (codePoint < 0x80) bytes.push(codePoint);
      else if (codePoint < 0x800) bytes.push(0xC0 | (codePoint >> 6), 0x80 | (codePoint & 63));
      else if (codePoint < 0x10000) bytes.push(0xE0 | (codePoint >> 12), 0x80 | ((codePoint >> 6) & 63), 0x80 | (codePoint & 63));
      else bytes.push(0xF0 | (codePoint >> 18), 0x80 | ((codePoint >> 12) & 63), 0x80 | ((codePoint >> 6) & 63), 0x80 | (codePoint & 63));
    }
    return new Uint8Array(bytes);
  };
  globalThis.TextEncoder = class TextEncoder {
    encode(str = '') { return _encodeUtf8(String(str)); }
    get encoding() { return 'utf-8'; }
  };
}
if (typeof TextDecoder === 'undefined') {
  globalThis.TextDecoder = class TextDecoder {
    constructor() {}
    decode(buf) { if (!buf) return ''; const u8 = new Uint8Array(buf.buffer || buf); let s = ''; for (let i = 0; i < u8.length;) { const b = u8[i++]; if (b < 128) s += String.fromCharCode(b); else if (b < 224) s += String.fromCharCode(((b&31)<<6)|(u8[i++]&63)); else if (b < 240) { const b2 = u8[i++]; s += String.fromCharCode(((b&15)<<12)|((b2&63)<<6)|(u8[i++]&63)); } else { const b2 = u8[i++], b3 = u8[i++], cp = ((b&7)<<18)|((b2&63)<<12)|((b3&63)<<6)|(u8[i++]&63); if (cp>0xFFFF) { const s2 = cp-0x10000; s += String.fromCharCode(0xD800+(s2>>10), 0xDC00+(s2&0x3FF)); } else s += String.fromCharCode(cp); } } return s; }
    get encoding() { return 'utf-8'; }
  };
}
if (typeof URL === 'undefined') {
  globalThis.URL = class URL {
    constructor(url, base) { const m = String(base ? new URL(base).href : ''); const full = url.startsWith('http') ? url : m.replace(/\\/[^\\/]*$/, '/') + url; const pm = full.match(/^(\\w+:)\\/\\/([^/:]+)(:\\d+)?(.*)$/); this.protocol = pm?.[1]||''; this.hostname = pm?.[2]||''; this.port = (pm?.[3]||'').slice(1); this.pathname = (pm?.[4]||'/').split('?')[0].split('#')[0]; this.search = full.includes('?') ? '?'+full.split('?')[1].split('#')[0] : ''; this.hash = full.includes('#') ? '#'+full.split('#')[1] : ''; this.host = this.hostname + (this.port ? ':'+this.port : ''); this.href = this.protocol+'//'+this.host+this.pathname+this.search+this.hash; this.origin = this.protocol+'//'+this.host; this.searchParams = typeof URLSearchParams !== 'undefined' ? new URLSearchParams(this.search) : { get:()=>null }; }
    toString() { return this.href; }
  };
}
if (typeof URLSearchParams === 'undefined') {
  globalThis.URLSearchParams = class URLSearchParams {
    constructor(init) { this._map = new Map(); if (typeof init === 'string') { for (const p of init.replace(/^\\?/,'').split('&')) { const [k,...v] = p.split('='); if (k) this._map.set(decodeURIComponent(k), decodeURIComponent(v.join('='))); } } }
    get(k) { return this._map.get(k) ?? null; }
    has(k) { return this._map.has(k); }
    toString() { return [...this._map].map(([k,v])=>encodeURIComponent(k)+'='+encodeURIComponent(v)).join('&'); }
  };
}
if (typeof structuredClone === 'undefined') {
  globalThis.structuredClone = (obj) => JSON.parse(JSON.stringify(obj));
}
if (typeof performance === 'undefined') {
  globalThis.performance = { now: () => Date.now(), timeOrigin: Date.now() };
}
if (
  typeof AbortController === 'undefined' ||
  typeof AbortSignal === 'undefined' ||
  typeof AbortSignal.prototype?.addEventListener !== 'function' ||
  typeof AbortSignal.prototype?.removeEventListener !== 'function'
) {
  const abortSignalState = new WeakMap();
  function getAbortSignalState(signal) {
    const state = abortSignalState.get(signal);
    if (!state) throw new Error('Invalid AbortSignal');
    return state;
  }
  class AbortSignal {
    constructor() {
      this.onabort = null;
      abortSignalState.set(this, {
        aborted: false,
        reason: undefined,
        listeners: [],
      });
    }
    get aborted() {
      return getAbortSignalState(this).aborted;
    }
    get reason() {
      return getAbortSignalState(this).reason;
    }
    get _listeners() {
      return getAbortSignalState(this).listeners.slice();
    }
    getEventListeners(type) {
      if (type !== 'abort') return [];
      return getAbortSignalState(this).listeners.slice();
    }
    addEventListener(type, listener) {
      if (type !== 'abort' || typeof listener !== 'function') return;
      getAbortSignalState(this).listeners.push(listener);
    }
    removeEventListener(type, listener) {
      if (type !== 'abort' || typeof listener !== 'function') return;
      const listeners = getAbortSignalState(this).listeners;
      const index = listeners.indexOf(listener);
      if (index !== -1) {
        listeners.splice(index, 1);
      }
    }
    dispatchEvent(event) {
      if (!event || event.type !== 'abort') return false;
      if (typeof this.onabort === 'function') {
        try {
          this.onabort.call(this, event);
        } catch {}
      }
      const listeners = getAbortSignalState(this).listeners.slice();
      for (const listener of listeners) {
        try {
          listener.call(this, event);
        } catch {}
      }
      return true;
    }
  }
  globalThis.AbortSignal = AbortSignal;
  globalThis.AbortController = class AbortController {
    constructor() {
      this.signal = new AbortSignal();
    }
    abort(reason) {
      const state = getAbortSignalState(this.signal);
      if (state.aborted) return;
      state.aborted = true;
      state.reason = reason;
      this.signal.dispatchEvent({ type: 'abort' });
    }
  };
}
if (
  typeof globalThis.AbortSignal === 'function' &&
  typeof globalThis.AbortController === 'function' &&
  typeof globalThis.AbortSignal.abort !== 'function'
) {
  globalThis.AbortSignal.abort = function abort(reason) {
    const controller = new globalThis.AbortController();
    controller.abort(reason);
    return controller.signal;
  };
}
if (
  typeof globalThis.AbortSignal === 'function' &&
  typeof globalThis.AbortController === 'function' &&
  typeof globalThis.AbortSignal.timeout !== 'function'
) {
  globalThis.AbortSignal.timeout = function timeout(milliseconds) {
    const delay = Number(milliseconds);
    if (!Number.isFinite(delay) || delay < 0) {
      throw new RangeError('The value of "milliseconds" is out of range. It must be a finite, non-negative number.');
    }
    const controller = new globalThis.AbortController();
    const timer = setTimeout(() => {
      controller.abort(
        new globalThis.DOMException(
          'The operation was aborted due to timeout',
          'TimeoutError',
        ),
      );
    }, delay);
    if (typeof timer?.unref === 'function') {
      timer.unref();
    }
    return controller.signal;
  };
}
if (
  typeof globalThis.AbortSignal === 'function' &&
  typeof globalThis.AbortController === 'function' &&
  typeof globalThis.AbortSignal.any !== 'function'
) {
  globalThis.AbortSignal.any = function any(signals) {
    if (
      signals === null ||
      signals === undefined ||
      typeof signals[Symbol.iterator] !== 'function'
    ) {
      throw new TypeError('The "signals" argument must be an iterable.');
    }

    const controller = new globalThis.AbortController();
    const cleanup = [];
    const abortFromSignal = (signal) => {
      for (const dispose of cleanup) {
        dispose();
      }
      cleanup.length = 0;
      controller.abort(signal.reason);
    };

    for (const signal of signals) {
      if (
        !signal ||
        typeof signal.aborted !== 'boolean' ||
        typeof signal.addEventListener !== 'function' ||
        typeof signal.removeEventListener !== 'function'
      ) {
        throw new TypeError('The "signals" argument must contain only AbortSignal instances.');
      }
      if (signal.aborted) {
        abortFromSignal(signal);
        break;
      }
      const listener = () => {
        abortFromSignal(signal);
      };
      signal.addEventListener('abort', listener, { once: true });
      cleanup.push(() => {
        signal.removeEventListener('abort', listener);
      });
    }

    return controller.signal;
  };
}
if (typeof navigator === 'undefined') {
  globalThis.navigator = { userAgent: 'secure-exec-v8' };
}
if (typeof DOMException === 'undefined') {
  const DOM_EXCEPTION_LEGACY_CODES = {
    IndexSizeError: 1,
    DOMStringSizeError: 2,
    HierarchyRequestError: 3,
    WrongDocumentError: 4,
    InvalidCharacterError: 5,
    NoDataAllowedError: 6,
    NoModificationAllowedError: 7,
    NotFoundError: 8,
    NotSupportedError: 9,
    InUseAttributeError: 10,
    InvalidStateError: 11,
    SyntaxError: 12,
    InvalidModificationError: 13,
    NamespaceError: 14,
    InvalidAccessError: 15,
    ValidationError: 16,
    TypeMismatchError: 17,
    SecurityError: 18,
    NetworkError: 19,
    AbortError: 20,
    URLMismatchError: 21,
    QuotaExceededError: 22,
    TimeoutError: 23,
    InvalidNodeTypeError: 24,
    DataCloneError: 25,
  };
  class DOMException extends Error {
    constructor(message = '', name = 'Error') {
      super(String(message));
      this.name = String(name);
      this.code = DOM_EXCEPTION_LEGACY_CODES[this.name] ?? 0;
    }
    get [Symbol.toStringTag]() { return 'DOMException'; }
  }
  for (const [name, code] of Object.entries(DOM_EXCEPTION_LEGACY_CODES)) {
    const constantName = name.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toUpperCase();
    Object.defineProperty(DOMException, constantName, {
      value: code,
      writable: false,
      configurable: false,
      enumerable: true,
    });
    Object.defineProperty(DOMException.prototype, constantName, {
      value: code,
      writable: false,
      configurable: false,
      enumerable: true,
    });
  }
  Object.defineProperty(globalThis, 'DOMException', {
    value: DOMException,
    writable: false,
    configurable: false,
    enumerable: true,
  });
}
if (typeof Blob === 'undefined') {
  globalThis.Blob = class Blob {
    constructor(parts = [], options = {}) {
      this._parts = Array.isArray(parts) ? parts.slice() : [];
      this.type = options && options.type ? String(options.type).toLowerCase() : '';
      this.size = this._parts.reduce((total, part) => {
        if (typeof part === 'string') return total + part.length;
        if (part && typeof part.byteLength === 'number') return total + part.byteLength;
        return total;
      }, 0);
    }
    arrayBuffer() { return Promise.resolve(new ArrayBuffer(0)); }
    text() { return Promise.resolve(''); }
    slice() { return new globalThis.Blob(); }
    stream() { throw new Error('Blob.stream is not supported in sandbox'); }
    get [Symbol.toStringTag]() { return 'Blob'; }
  };
  Object.defineProperty(globalThis, 'Blob', {
    value: globalThis.Blob,
    writable: false,
    configurable: false,
    enumerable: true,
  });
}
if (typeof File === 'undefined') {
  globalThis.File = class File extends globalThis.Blob {
    constructor(parts = [], name = '', options = {}) {
      super(parts, options);
      this.name = String(name);
      this.lastModified =
        options && typeof options.lastModified === 'number'
          ? options.lastModified
          : Date.now();
      this.webkitRelativePath = '';
    }
    get [Symbol.toStringTag]() { return 'File'; }
  };
  Object.defineProperty(globalThis, 'File', {
    value: globalThis.File,
    writable: false,
    configurable: false,
    enumerable: true,
  });
}
if (typeof FormData === 'undefined') {
  class FormData {
    constructor() {
      this._entries = [];
    }
    append(name, value) {
      this._entries.push([String(name), value]);
    }
    get(name) {
      const key = String(name);
      for (const entry of this._entries) {
        if (entry[0] === key) return entry[1];
      }
      return null;
    }
    getAll(name) {
      const key = String(name);
      return this._entries.filter((entry) => entry[0] === key).map((entry) => entry[1]);
    }
    has(name) {
      return this.get(name) !== null;
    }
    delete(name) {
      const key = String(name);
      this._entries = this._entries.filter((entry) => entry[0] !== key);
    }
    entries() {
      return this._entries[Symbol.iterator]();
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    get [Symbol.toStringTag]() { return 'FormData'; }
  }
  Object.defineProperty(globalThis, 'FormData', {
    value: FormData,
    writable: false,
    configurable: false,
    enumerable: true,
  });
}
if (typeof MessageEvent === 'undefined') {
  globalThis.MessageEvent = class MessageEvent {
    constructor(type, options = {}) {
      this.type = String(type);
      this.data = Object.prototype.hasOwnProperty.call(options, 'data')
        ? options.data
        : undefined;
    }
  };
}
if (typeof MessagePort === 'undefined') {
  globalThis.MessagePort = class MessagePort {
    constructor() {
      this.onmessage = null;
      this._pairedPort = null;
    }
    postMessage(data) {
      const target = this._pairedPort;
      if (!target) return;
      const event = new globalThis.MessageEvent('message', { data });
      if (typeof target.onmessage === 'function') {
        target.onmessage.call(target, event);
      }
    }
    start() {}
    close() {
      this._pairedPort = null;
    }
  };
}
if (typeof MessageChannel === 'undefined') {
  globalThis.MessageChannel = class MessageChannel {
    constructor() {
      this.port1 = new globalThis.MessagePort();
      this.port2 = new globalThis.MessagePort();
      this.port1._pairedPort = this.port2;
      this.port2._pairedPort = this.port1;
    }
  };
}
`;

// Shim for ivm.Reference methods used by bridge code.
// Bridge globals in the V8 runtime are plain functions, but the bridge code
// (compiled from @secure-exec/core) calls them via .applySync(), .apply(), and
// .applySyncPromise() which are ivm Reference calling patterns.
// Shim for native bridge functions (runs early in postRestoreScript)
const BRIDGE_NATIVE_SHIM = `
(function() {
  var _origApply = Function.prototype.apply;
  function shimBridgeGlobal(name) {
    var fn = globalThis[name];
    if (typeof fn !== 'function' || fn.applySync) return;
    fn.applySync = function(_, args) { return _origApply.call(fn, null, args || []); };
    fn.applySyncPromise = function(_, args) { return _origApply.call(fn, null, args || []); };
    fn.derefInto = function() { return fn; };
  }
  var keys = Object.getOwnPropertyNames(globalThis).filter(function(k) { return k.startsWith('_') && typeof globalThis[k] === 'function'; });
  keys.forEach(shimBridgeGlobal);
})();
`;

// Dispatch shim for bridge globals not natively supported by the V8 binary.
// Installs dispatch wrappers for ALL known bridge globals that aren't already
// functions. This runs BEFORE require-setup so the crypto/net module code
// detects the dispatch-wrapped globals and installs the corresponding APIs.
function buildBridgeDispatchShim(): string {
	const K = HOST_BRIDGE_GLOBAL_KEYS;
	// Collect all bridge global names from the contract
	const allGlobals = Object.values(K).filter(v => typeof v === "string") as string[];
	return `
(function() {
  var _origApply = Function.prototype.apply;
  function encodeDispatchArgs(args) {
    return JSON.stringify(args, function(_key, value) {
      if (value === undefined) {
        return { __secureExecDispatchType: 'undefined' };
      }
      return value;
    });
  }
  var names = ${JSON.stringify(allGlobals)};
  for (var i = 0; i < names.length; i++) {
    var name = names[i];
    if (typeof globalThis[name] === 'function') continue;
    (function(n) {
      function reviveDispatchError(payload) {
        var error = new Error(payload && payload.message ? payload.message : String(payload));
        if (payload && payload.name) error.name = payload.name;
        if (payload && payload.code !== undefined) error.code = payload.code;
        if (payload && payload.stack) error.stack = payload.stack;
        return error;
      }
      var fn = function() {
        var args = Array.prototype.slice.call(arguments);
        var encoded = "__bd:" + n + ":" + encodeDispatchArgs(args);
        var resultJson = _loadPolyfill.applySyncPromise(undefined, [encoded]);
        if (resultJson === null) return undefined;
        try {
          var parsed = JSON.parse(resultJson);
          if (parsed.__bd_error) throw reviveDispatchError(parsed.__bd_error);
          return parsed.__bd_result;
        } catch (e) {
          if (e.message && e.message.startsWith('No handler:')) return undefined;
          throw e;
        }
      };
      fn.applySync = function(_, args) { return _origApply.call(fn, null, args || []); };
      fn.applySyncPromise = function(_, args) { return _origApply.call(fn, null, args || []); };
      fn.derefInto = function() { return fn; };
      globalThis[n] = fn;
    })(name);
  }
})();
`;
}
const BRIDGE_DISPATCH_SHIM = buildBridgeDispatchShim();

// Cache assembled bridge code (same across all executions)
let bridgeCodeCache: string | null = null;

function buildFullBridgeCode(): string {
	if (bridgeCodeCache) return bridgeCodeCache;

	// Assemble the full bridge code IIFE from component scripts.
	// Only include code that can run without bridge calls (snapshot phase).
	// Console/require/fsFacade setup goes in postRestoreScript where bridge calls work.
	const parts = [
		// Polyfill missing Web APIs for the Rust V8 runtime
		V8_POLYFILLS,
		getIsolateRuntimeSource("globalExposureHelpers"),
		getInitialBridgeGlobalsSetupCode(),
		getRawBridgeCode(),
		getBridgeAttachCode(),
	];

	bridgeCodeCache = parts.join("\n");
	return bridgeCodeCache;
}

export class NodeExecutionDriver implements RuntimeDriver {
	private state: DriverState;
	private memoryLimit: number;
	private disposed: boolean = false;
	private flattenedBindings: FlattenedBinding[] | null = null;
	// Unwrapped filesystem for path translation (toHostPath/toSandboxPath)
	private rawFilesystem: VirtualFileSystem | undefined;
	// Kernel socket table for routing net.connect through kernel
	private socketTable?: import("@secure-exec/core").SocketTable;
	// Kernel process table for child process registration
	private processTable?: import("@secure-exec/core").ProcessTable;
	private timerTable: import("@secure-exec/core").TimerTable;
	private ownsProcessTable: boolean;
	private ownsTimerTable: boolean;
	private configuredMaxTimers?: number;
	private configuredMaxHandles?: number;
	private pid?: number;
	// Track the current V8 session so it can be destroyed on terminate/dispose
	private _currentSession: V8Session | null = null;

	constructor(options: NodeExecutionDriverOptions) {
		sharedV8RuntimeUsers += 1;
		this.memoryLimit = options.memoryLimit ?? 128;
		const budgets = options.resourceBudgets;
		this.socketTable = options.socketTable;
		this.processTable = options.processTable ?? new ProcessTable();
		this.timerTable = options.timerTable ?? new TimerTable();
		this.ownsProcessTable = options.processTable === undefined;
		this.ownsTimerTable = options.timerTable === undefined;
		this.configuredMaxTimers = budgets?.maxTimers;
		this.configuredMaxHandles = budgets?.maxHandles;
		this.pid = options.pid ?? 1;
		const system = options.system;
		const permissions = system.permissions;
		if (!this.socketTable) {
			this.socketTable = new SocketTable({
				hostAdapter: system.network ? createNodeHostNetworkAdapter() : undefined,
				networkCheck: permissions?.network,
			});
		}
		// Keep unwrapped filesystem for path translation (toHostPath/toSandboxPath)
		this.rawFilesystem = system.filesystem;
		const filesystem = this.rawFilesystem
			? wrapFileSystem(this.rawFilesystem, permissions)
			: createFsStub();
		const commandExecutor = system.commandExecutor
			? wrapCommandExecutor(system.commandExecutor, permissions)
			: createCommandExecutorStub();
		const rawNetworkAdapter = system.network;
		const networkAdapter = rawNetworkAdapter
			? wrapNetworkAdapter(rawNetworkAdapter, permissions)
			: createNetworkStub();
		const loopbackAwareAdapter = networkAdapter as LoopbackAwareNetworkAdapter;
		if (loopbackAwareAdapter.__setLoopbackPortChecker && this.socketTable) {
			loopbackAwareAdapter.__setLoopbackPortChecker((_hostname, port) =>
				this.socketTable?.findListener({ host: "127.0.0.1", port }) !== null,
			);
		}

		const processConfig = { ...(options.runtime.process ?? {}) };
		processConfig.cwd ??= DEFAULT_SANDBOX_CWD;
		processConfig.env = filterEnv(processConfig.env, permissions);

		const osConfig = { ...(options.runtime.os ?? {}) };
		osConfig.homedir ??= DEFAULT_SANDBOX_HOME;
		osConfig.tmpdir ??= DEFAULT_SANDBOX_TMPDIR;

		const bridgeBase64TransferLimitBytes = normalizePayloadLimit(
			options.payloadLimits?.base64TransferBytes,
			DEFAULT_BRIDGE_BASE64_TRANSFER_BYTES,
			"payloadLimits.base64TransferBytes",
		);
		const isolateJsonPayloadLimitBytes = normalizePayloadLimit(
			options.payloadLimits?.jsonPayloadBytes,
			DEFAULT_ISOLATE_JSON_PAYLOAD_BYTES,
			"payloadLimits.jsonPayloadBytes",
		);

		this.state = {
			filesystem,
			commandExecutor,
			networkAdapter,
			permissions,
			processConfig,
			osConfig,
			onStdio: options.onStdio,
			cpuTimeLimitMs: options.cpuTimeLimitMs,
			timingMitigation: options.timingMitigation ?? "freeze",
			bridgeBase64TransferLimitBytes,
			isolateJsonPayloadLimitBytes,
			maxOutputBytes: budgets?.maxOutputBytes,
			maxBridgeCalls: budgets?.maxBridgeCalls,
			maxChildProcesses: budgets?.maxChildProcesses,
			maxTimers: budgets?.maxTimers,
			maxHandles: budgets?.maxHandles,
			budgetState: createBudgetState(),
			activeHttpServerIds: new Set(),
			activeHttpServerClosers: new Map(),
			pendingHttpServerStarts: { count: 0 },
			activeHttpClientRequests: { count: 0 },
			activeChildProcesses: new Map(),
			activeHostTimers: new Set(),
			moduleFormatCache: new Map(),
			packageTypeCache: new Map(),
			resolutionCache: createResolutionCache(),
			onPtySetRawMode: options.onPtySetRawMode,
			liveStdinSource: options.liveStdinSource,
		};

		// Validate and flatten bindings once at construction time
		if (options.bindings) {
			this.flattenedBindings = flattenBindingTree(options.bindings);
		}
	}

	get network(): Pick<NetworkAdapter, "fetch" | "dnsLookup" | "httpRequest"> {
		const adapter = this.state.networkAdapter ?? createNetworkStub();
		return {
			fetch: (url, options) => adapter.fetch(url, options),
			dnsLookup: (hostname) => adapter.dnsLookup(hostname),
			httpRequest: (url, options) => adapter.httpRequest(url, options),
		};
	}

	get unsafeIsolate(): unknown { return null; }

	private hasManagedResources(): boolean {
		const hasBridgeHandles =
			this.pid !== undefined &&
			this.processTable !== undefined &&
			(() => {
				try {
					return this.processTable.getHandles(this.pid!).size > 0;
				} catch {
					return false;
				}
			})();
		return (
			hasBridgeHandles ||
			this.state.pendingHttpServerStarts.count > 0 ||
			this.state.activeHttpClientRequests.count > 0 ||
			this.state.activeHttpServerIds.size > 0 ||
			this.state.activeChildProcesses.size > 0 ||
			(!this.ownsProcessTable && this.state.activeHostTimers.size > 0)
		);
	}

	private async waitForManagedResources(): Promise<void> {
		const graceDeadline = Date.now() + 100;

		// Give async bridge callbacks a moment to register their host-side handles.
		while (!this.disposed && !this.hasManagedResources() && Date.now() < graceDeadline) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}

		// Keep the session alive while host-managed resources are still active.
		while (!this.disposed && this.hasManagedResources()) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
	}

	private ensureBridgeProcessEntry(processConfig: ProcessConfig): void {
		if (this.pid === undefined || !this.processTable) return;

		const entry = this.processTable.get(this.pid);
		if (!entry || entry.status === "exited") {
			this.processTable.register(
				this.pid,
				"node",
				"node",
				[],
				{
					pid: this.pid,
					ppid: 0,
					env: processConfig.env ?? {},
					cwd: processConfig.cwd ?? DEFAULT_SANDBOX_CWD,
					fds: { stdin: 0, stdout: 1, stderr: 2 },
					stdinIsTTY: processConfig.stdinIsTTY,
					stdoutIsTTY: processConfig.stdoutIsTTY,
					stderrIsTTY: processConfig.stderrIsTTY,
				},
				createBridgeDriverProcess(),
			);
		}

		if (this.ownsProcessTable || this.configuredMaxHandles !== undefined) {
			this.processTable.setHandleLimit(
				this.pid,
				this.configuredMaxHandles ?? DEFAULT_MAX_HANDLES,
			);
		}

		if (this.ownsTimerTable || this.configuredMaxTimers !== undefined) {
			this.timerTable.setLimit(
				this.pid,
				this.configuredMaxTimers ?? DEFAULT_MAX_TIMERS,
			);
		}
	}

	private clearKernelTimersForProcess(pid: number): void {
		for (const timer of this.timerTable.getActiveTimers(pid)) {
			if (timer.hostHandle !== undefined) {
				clearTimeout(timer.hostHandle as ReturnType<typeof setTimeout>);
				this.state.activeHostTimers.delete(
					timer.hostHandle as ReturnType<typeof setTimeout>,
				);
				timer.hostHandle = undefined;
			}
			this.timerTable.clearTimer(timer.id);
		}
	}

	private finalizeExecutionState(exitCode: number): void {
		if (this.pid === undefined) return;
		this.clearKernelTimersForProcess(this.pid);
		if (this.ownsProcessTable && this.processTable) {
			this.processTable.markExited(this.pid, exitCode);
		}
	}

	async createUnsafeContext(_options: { env?: Record<string, string>; cwd?: string; filePath?: string } = {}): Promise<unknown> {
		return null;
	}

	async run<T = unknown>(code: string, filePath?: string): Promise<RunResult<T>> {
		return this.executeInternal<T>({ mode: "run", code, filePath });
	}

	async exec(code: string, options?: ExecOptions): Promise<ExecResult> {
		const result = await this.executeInternal({
			mode: options?.mode ?? "exec",
			code,
			filePath: options?.filePath,
			env: options?.env,
			cwd: options?.cwd,
			stdin: options?.stdin,
			cpuTimeLimitMs: options?.cpuTimeLimitMs,
			timingMitigation: options?.timingMitigation,
			onStdio: options?.onStdio,
		});
		return { code: result.code, errorMessage: result.errorMessage };
	}

	private async executeInternal<T = unknown>(options: {
		mode: "run" | "exec";
		code: string;
		filePath?: string;
		env?: Record<string, string>;
		cwd?: string;
		stdin?: string;
		cpuTimeLimitMs?: number;
		timingMitigation?: TimingMitigation;
		onStdio?: StdioHook;
	}): Promise<RunResult<T>> {
		if (this.disposed) throw new Error("NodeExecutionDriver has been disposed");

		// Reset per-execution state
		this.state.budgetState = createBudgetState();
		this.state.moduleFormatCache.clear();
		this.state.packageTypeCache.clear();
		this.state.resolutionCache.resolveResults.clear();
		this.state.resolutionCache.packageJsonResults.clear();
		this.state.resolutionCache.existsResults.clear();
		this.state.resolutionCache.statResults.clear();

		const s = this.state;
		const timingMitigation = getTimingMitigation(options.timingMitigation, s.timingMitigation);
		const frozenTimeMs = Date.now();
		const onStdio = options.onStdio ?? s.onStdio;
		const entryIsEsm = await shouldRunAsESM(
			{
				filesystem: s.filesystem,
				packageTypeCache: s.packageTypeCache,
				moduleFormatCache: s.moduleFormatCache,
				isolateJsonPayloadLimitBytes: s.isolateJsonPayloadLimitBytes,
				resolutionCache: s.resolutionCache,
			},
			options.code,
			options.filePath,
		);
		// For ESM entry scripts, wrap in a CJS dynamic import() launcher instead
		// of using V8's native ESM "run" mode. The native ESM module resolver in
		// the V8 isolate doesn't resolve non-builtin packages (undici, chalk, etc.)
		// through the bridge. Dynamic import() in CJS mode goes through the
		// require-setup's __dynamicImportHandler which uses _requireFrom and
		// properly resolves packages via the bridge.
		// For ESM entry scripts loaded from module access overlay paths,
		// use CJS exec mode with a dynamic import() wrapper instead of V8's
		// native ESM "run" mode. The native ESM resolver makes a synchronous
		// IPC call per import, which is prohibitively slow for packages with
		const sessionMode = options.mode === "run" || entryIsEsm ? "run" : "exec";
		const userCode = entryIsEsm
			? options.code
			: (() => {
					const transformed = transformSourceForRequireSync(
						options.code,
						options.filePath ?? "/entry.js",
					);
					if (options.mode !== "exec") {
						return transformed;
					}
					return `${transformed}\n;typeof _waitForActiveHandles === "function" ? _waitForActiveHandles() : undefined;`;
				})();

		// Get or create V8 runtime
		const v8Runtime = await getSharedV8Runtime();
		const cpuTimeLimitMs = getExecutionTimeoutMs(options.cpuTimeLimitMs, s.cpuTimeLimitMs);

		const sessionOpts: V8SessionOptions = {
			heapLimitMb: this.memoryLimit,
			cpuTimeLimitMs,
		};
		const session = await v8Runtime.createSession(sessionOpts);
		let finalExitCode = 0;

		try {
			const execProcessConfig = createProcessConfigForExecution(
				options.env || options.cwd
					? {
							...s.processConfig,
							...(options.env ? { env: filterEnv(options.env, s.permissions) } : {}),
							...(options.cwd ? { cwd: options.cwd } : {}),
						}
					: s.processConfig,
				timingMitigation,
				frozenTimeMs,
			);
			this.ensureBridgeProcessEntry(execProcessConfig);

			// Build bridge handlers for this execution
			const cryptoResult = buildCryptoBridgeHandlers();
			const sendStreamEvent = (eventType: string, payload: Uint8Array) => {
				try {
					session.sendStreamEvent(eventType, payload);
				} catch {
					// Session may be destroyed
				}
			};

			const netSocketResult = buildNetworkSocketBridgeHandlers({
				dispatch: (socketId, event, data) => {
					const payload = JSON.stringify({ socketId, event, data });
					sendStreamEvent("netSocket", Buffer.from(payload));
				},
				socketTable: this.socketTable,
				pid: this.pid,
			});

			const networkBridgeResult = buildNetworkBridgeHandlers({
				networkAdapter: s.networkAdapter,
				budgetState: s.budgetState,
				maxBridgeCalls: s.maxBridgeCalls,
				isolateJsonPayloadLimitBytes: s.isolateJsonPayloadLimitBytes,
				activeHttpServerIds: s.activeHttpServerIds,
				activeHttpServerClosers: s.activeHttpServerClosers,
				pendingHttpServerStarts: s.pendingHttpServerStarts,
				activeHttpClientRequests: s.activeHttpClientRequests,
				sendStreamEvent,
				socketTable: this.socketTable,
				pid: this.pid,
			});

			const kernelFdResult = buildKernelFdBridgeHandlers({
				filesystem: s.filesystem,
				budgetState: s.budgetState,
				maxBridgeCalls: s.maxBridgeCalls,
			});
			const kernelTimerDispatchHandlers = buildKernelTimerDispatchHandlers({
				timerTable: this.timerTable,
				pid: this.pid ?? 1,
				budgetState: s.budgetState,
				maxBridgeCalls: s.maxBridgeCalls,
				activeHostTimers: s.activeHostTimers,
				sendStreamEvent,
			});
			const kernelHandleDispatchHandlers = buildKernelHandleDispatchHandlers({
				processTable: this.processTable,
				pid: this.pid ?? 1,
				budgetState: s.budgetState,
				maxBridgeCalls: s.maxBridgeCalls,
			});
			const kernelStdinDispatchHandlers = buildKernelStdinDispatchHandlers({
				liveStdinSource: s.liveStdinSource,
				budgetState: s.budgetState,
				maxBridgeCalls: s.maxBridgeCalls,
			});

			const bridgeHandlers: BridgeHandlers = {
				...cryptoResult.handlers,
				...buildConsoleBridgeHandlers({
					onStdio,
					budgetState: s.budgetState,
					maxOutputBytes: s.maxOutputBytes,
				}),
				...kernelStdinDispatchHandlers,
				...buildModuleLoadingBridgeHandlers({
					filesystem: s.filesystem,
					resolutionCache: s.resolutionCache,
					resolveMode: entryIsEsm ? "import" : "require",
					sandboxToHostPath: (p) => {
						const rfs = this.rawFilesystem as any;
						return typeof rfs?.toHostPath === "function" ? rfs.toHostPath(p) : null;
					},
				}, {
					// Dispatch handlers routed through _loadPolyfill for V8 runtime compat
					...cryptoResult.handlers,
					...networkBridgeResult.handlers,
					...netSocketResult.handlers,
					...buildModuleResolutionBridgeHandlers({
						sandboxToHostPath: (p) => {
							const fs = s.filesystem as any;
							return typeof fs.toHostPath === "function" ? fs.toHostPath(p) : null;
						},
						hostToSandboxPath: (p) => {
							const fs = s.filesystem as any;
							return typeof fs.toSandboxPath === "function" ? fs.toSandboxPath(p) : p;
						},
						additionalResolvePaths: (() => {
							// Collect host paths from package roots for transitive dep resolution.
							// Each package root's parent node_modules is a valid search path.
							const fs = s.filesystem as any;
							const roots: string[] = [];
							if (Array.isArray(fs?.packageRoots)) {
								for (const root of fs.packageRoots) {
									if (root.hostPath) roots.push(root.hostPath);
								}
							}
							// Also include moduleAccessCwd's node_modules
							if (fs?.hostNodeModulesRoot) {
								roots.push(fs.hostNodeModulesRoot);
							}
							return roots.length > 0 ? roots : undefined;
						})(),
					}),
					...buildPtyBridgeHandlers({
						onPtySetRawMode: s.onPtySetRawMode,
						stdinIsTTY: s.processConfig.stdinIsTTY,
					}),
					...buildMimeBridgeHandlers(),
					// Kernel FD table handlers
					...kernelFdResult.handlers,
					...kernelTimerDispatchHandlers,
					...kernelHandleDispatchHandlers,
					// Custom bindings dispatched through _loadPolyfill
					...(this.flattenedBindings ? Object.fromEntries(
						this.flattenedBindings.map(b => [b.key, b.handler])
					) : {}),
				}),
				...buildTimerBridgeHandlers({
					budgetState: s.budgetState,
					maxBridgeCalls: s.maxBridgeCalls,
					activeHostTimers: s.activeHostTimers,
				}),
				...buildFsBridgeHandlers({
					filesystem: s.filesystem,
					budgetState: s.budgetState,
					maxBridgeCalls: s.maxBridgeCalls,
					bridgeBase64TransferLimitBytes: s.bridgeBase64TransferLimitBytes,
					isolateJsonPayloadLimitBytes: s.isolateJsonPayloadLimitBytes,
				}),
				...buildChildProcessBridgeHandlers({
					commandExecutor: s.commandExecutor,
					processConfig: s.processConfig,
					budgetState: s.budgetState,
					maxBridgeCalls: s.maxBridgeCalls,
					maxChildProcesses: s.maxChildProcesses,
					isolateJsonPayloadLimitBytes: s.isolateJsonPayloadLimitBytes,
					activeChildProcesses: s.activeChildProcesses,
					sendStreamEvent,
					processTable: this.processTable,
					parentPid: this.pid,
				}),
				...networkBridgeResult.handlers,
				...netSocketResult.handlers,
				...buildModuleResolutionBridgeHandlers({
					sandboxToHostPath: (p) => {
						const rfs = this.rawFilesystem as any;
						return typeof rfs?.toHostPath === "function" ? rfs.toHostPath(p) : null;
					},
					hostToSandboxPath: (p) => {
						const rfs = this.rawFilesystem as any;
						return typeof rfs?.toSandboxPath === "function" ? rfs.toSandboxPath(p) : p;
					},
				}),
				...buildPtyBridgeHandlers({
					onPtySetRawMode: s.onPtySetRawMode,
					stdinIsTTY: s.processConfig.stdinIsTTY,
				}),
			};

			// Merge custom bindings into bridge handlers
			if (this.flattenedBindings) {
				for (const binding of this.flattenedBindings) {
					bridgeHandlers[binding.key] = binding.handler;
				}
			}

			// Build bridge code with embedded config
			const bridgeCode = buildFullBridgeCode();

			// Build post-restore script with per-execution config
			const bindingKeys = this.flattenedBindings
				? this.flattenedBindings.map((b) => b.key.slice(BINDING_PREFIX.length))
				: [];
			const postRestoreScript = buildPostRestoreScript(
				execProcessConfig,
				s.osConfig,
				{
					initialCwd: execProcessConfig.cwd ?? "/",
					jsonPayloadLimitBytes: s.isolateJsonPayloadLimitBytes,
					payloadLimitErrorCode: PAYLOAD_LIMIT_ERROR_CODE,
					maxTimers: s.maxTimers,
					maxHandles: s.maxHandles,
					stdin: options.stdin,
					streamStdin: !!s.liveStdinSource && !execProcessConfig.stdinIsTTY,
				},
				timingMitigation,
				frozenTimeMs,
				sessionMode,
				options.filePath,
				bindingKeys,
			);

			// Track session so terminate/dispose can destroy it
			this._currentSession = session;

			const sessionProcessConfig = {
				platform: execProcessConfig.platform,
				arch: execProcessConfig.arch,
				version: execProcessConfig.version,
				cwd: execProcessConfig.cwd ?? "/",
				env: execProcessConfig.env ?? {},
				argv: execProcessConfig.argv,
				execPath: execProcessConfig.execPath,
				pid: execProcessConfig.pid,
				ppid: execProcessConfig.ppid,
				uid: execProcessConfig.uid,
				gid: execProcessConfig.gid,
				stdin: execProcessConfig.stdin,
				timing_mitigation: timingMitigation,
				frozen_time_ms: timingMitigation === "freeze" ? frozenTimeMs : null,
			} as V8ExecutionOptions["processConfig"];

			// Execute in V8 session
			const result = await session.execute({
				bridgeCode,
				postRestoreScript,
				userCode,
				mode: sessionMode,
				filePath: options.filePath,
				processConfig: sessionProcessConfig,
				osConfig: {
					homedir: s.osConfig.homedir ?? DEFAULT_SANDBOX_HOME,
					tmpdir: s.osConfig.tmpdir ?? DEFAULT_SANDBOX_TMPDIR,
					platform: s.osConfig.platform ?? "linux",
					arch: s.osConfig.arch ?? "x64",
				},
				bridgeHandlers,
				onStreamCallback: (callbackType, payload) => {
					// Handle stream callbacks from V8 isolate
					if (callbackType === "httpServerResponse") {
						try {
							const data = JSON.parse(Buffer.from(payload).toString());
							resolveHttpServerResponse({
								requestId: data.requestId !== undefined
									? Number(data.requestId)
									: undefined,
								serverId: data.serverId !== undefined
									? Number(data.serverId)
									: undefined,
								responseJson: data.responseJson,
							});
						} catch {
							// Invalid payload
						}
					}
				},
			});

			if (options.mode === "exec" && !result.error) {
				await this.waitForManagedResources();
			}

			// Clean up per-execution resources
			cryptoResult.dispose();
			netSocketResult.dispose();
			kernelFdResult.dispose();
			await networkBridgeResult.dispose();

			// Map V8 execution result to RunResult
			if (result.error) {
				const errMessage = result.error.type && result.error.type !== "Error"
					? `${result.error.type}: ${result.error.message}`
					: result.error.message;

				// Check for timeout
				if (/timed out|time limit exceeded/i.test(errMessage)) {
					finalExitCode = TIMEOUT_EXIT_CODE;
					return {
						code: TIMEOUT_EXIT_CODE,
						errorMessage: TIMEOUT_ERROR_MESSAGE,
						exports: undefined as T,
					};
				}

				// Check for process.exit()
				const exitMatch = errMessage.match(/process\.exit\((\d+)\)/);
				if (exitMatch) {
					finalExitCode = parseInt(exitMatch[1], 10);
					return {
						code: finalExitCode,
						exports: undefined as T,
					};
				}

				finalExitCode = result.code || 1;
				return {
					code: finalExitCode,
					errorMessage: boundErrorMessage(errMessage),
					exports: undefined as T,
				};
			}

			// Parse exports for run() mode
			let exports: T | undefined;
			if (options.mode === "run" && result.exports) {
				try {
					if (typeof (globalThis as Record<string, unknown>).Bun !== "undefined") {
						// Bun: CBOR codec — payload is CBOR bytes
						// eslint-disable-next-line @typescript-eslint/no-require-imports
						const cbor = require("cbor-x") as typeof import("cbor-x");
						exports = cbor.decode(Buffer.from(result.exports)) as T;
					} else {
						const { deserialize } = await import("node:v8");
						exports = deserialize(result.exports) as T;
					}
				} catch {
					exports = undefined;
				}
			}

			finalExitCode = result.code;
			return {
				code: finalExitCode,
				exports,
			};
		} catch (err) {
			const errMessage = err instanceof Error
				? (err.name && err.name !== "Error" ? `${err.name}: ${err.message}` : err.message)
				: String(err);

			if (/timed out|time limit exceeded/i.test(errMessage)) {
				finalExitCode = TIMEOUT_EXIT_CODE;
				return {
					code: TIMEOUT_EXIT_CODE,
					errorMessage: TIMEOUT_ERROR_MESSAGE,
					exports: undefined as T,
				};
			}

			const exitMatch = errMessage.match(/process\.exit\((\d+)\)/);
			if (exitMatch) {
				finalExitCode = parseInt(exitMatch[1], 10);
				return {
					code: finalExitCode,
					exports: undefined as T,
				};
			}

			finalExitCode = 1;
			return {
				code: finalExitCode,
				errorMessage: boundErrorMessage(errMessage),
				exports: undefined as T,
			};
		} finally {
			this._currentSession = null;
			await session.destroy().catch(() => {});
			this.finalizeExecutionState(finalExitCode);
		}
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		// Destroy V8 session to unregister handler and unref IPC socket
		if (this._currentSession) {
			this._currentSession.destroy().catch(() => {});
			this._currentSession = null;
		}
		killActiveChildProcesses(this.state);
		clearActiveHostTimers(this.state);
		if (this.pid !== undefined) {
			this.clearKernelTimersForProcess(this.pid);
		}
		void releaseSharedV8Runtime();
	}

	async terminate(): Promise<void> {
		if (this.disposed) return;
		// Destroy V8 session to unregister handler and unref IPC socket
		if (this._currentSession) {
			await this._currentSession.destroy().catch(() => {});
			this._currentSession = null;
		}
		killActiveChildProcesses(this.state);
		const closers = Array.from(this.state.activeHttpServerClosers.values());
		await Promise.allSettled(closers.map((close) => close()));
		this.state.activeHttpServerIds.clear();
		this.state.activeHttpServerClosers.clear();
		clearActiveHostTimers(this.state);
		if (this.pid !== undefined) {
			this.clearKernelTimersForProcess(this.pid);
		}
		this.disposed = true;
		await releaseSharedV8Runtime();
	}
}

/** Build the post-restore script that configures the V8 session per-execution. */
function buildPostRestoreScript(
	processConfig: ProcessConfig,
	osConfig: OSConfig,
	bridgeConfig: {
		initialCwd: string;
		jsonPayloadLimitBytes: number;
		payloadLimitErrorCode: string;
		maxTimers?: number;
		maxHandles?: number;
		stdin?: string;
		streamStdin?: boolean;
	},
	timingMitigation: TimingMitigation,
	frozenTimeMs: number,
	mode: "run" | "exec",
	filePath?: string,
	bindingKeys?: string[],
): string {
	const parts: string[] = [];

	// Shim existing native bridge functions for ivm.Reference compat,
	// then install dispatch wrappers for bridge globals not in the V8 binary
	parts.push(BRIDGE_NATIVE_SHIM);
	parts.push(BRIDGE_DISPATCH_SHIM);

	// Console and require setup (must run in postRestoreScript, not bridgeCode,
	// because bridge calls are muted during the bridgeCode snapshot phase)
	parts.push(getConsoleSetupCode());
	parts.push(getRequireSetupCode());
	parts.push(getIsolateRuntimeSource("setupFsFacade"));
	parts.push(`globalThis.__runtimeDynamicImportConfig = ${JSON.stringify({
		referrerPath: filePath ?? processConfig.cwd ?? bridgeConfig.initialCwd,
	})};`);
	parts.push(getIsolateRuntimeSource("setupDynamicImport"));

	// Inject bridge setup config
	parts.push(`globalThis.__runtimeBridgeSetupConfig = ${JSON.stringify({
		initialCwd: bridgeConfig.initialCwd,
		jsonPayloadLimitBytes: bridgeConfig.jsonPayloadLimitBytes,
		payloadLimitErrorCode: bridgeConfig.payloadLimitErrorCode,
	})};`);

	// Inject process and OS config
	parts.push(`globalThis.${getProcessConfigGlobalKey()} = ${JSON.stringify(processConfig)};`);
	parts.push(`globalThis.${getOsConfigGlobalKey()} = ${JSON.stringify(osConfig)};`);
	parts.push(`if (typeof globalThis.__runtimeRefreshProcessConfig === "function") globalThis.__runtimeRefreshProcessConfig();`);

	// Inject TTY config separately — InjectGlobals overwrites _processConfig,
	// so TTY flags need their own global that persists
	if (processConfig.stdinIsTTY || processConfig.stdoutIsTTY || processConfig.stderrIsTTY
		|| processConfig.cols || processConfig.rows) {
		parts.push(`globalThis.__runtimeTtyConfig = ${JSON.stringify({
			stdinIsTTY: processConfig.stdinIsTTY,
			stdoutIsTTY: processConfig.stdoutIsTTY,
			stderrIsTTY: processConfig.stderrIsTTY,
			cols: processConfig.cols,
			rows: processConfig.rows,
		})};`);
	}

	// Enable streaming stdin for non-TTY processes that need live stdin delivery
	if (bridgeConfig.streamStdin) {
		parts.push(`globalThis.__runtimeStreamStdin = true;`);
	}

	// Inject timer/handle limits
	if (bridgeConfig.maxTimers !== undefined) {
		parts.push(`globalThis._maxTimers = ${bridgeConfig.maxTimers};`);
	}
	if (bridgeConfig.maxHandles !== undefined) {
		parts.push(`globalThis._maxHandles = ${bridgeConfig.maxHandles};`);
	}

	// Apply timing mitigation
	if (timingMitigation === "freeze") {
		parts.push(`globalThis.__runtimeTimingMitigationConfig = ${JSON.stringify({ frozenTimeMs })};`);
		parts.push(getIsolateRuntimeSource("applyTimingMitigationFreeze"));
	} else {
		parts.push(getIsolateRuntimeSource("applyTimingMitigationOff"));
	}

	// Apply env, cwd, and stdin overrides for all modes.
	// These must run even in "run" (ESM) mode so that process.env and
	// process.cwd() reflect the spawn-time configuration.
	if (processConfig.env) {
		parts.push(`globalThis.__runtimeProcessEnvOverride = ${JSON.stringify(processConfig.env)};`);
		parts.push(getIsolateRuntimeSource("overrideProcessEnv"));
	}
	if (processConfig.cwd) {
		parts.push(`globalThis.__runtimeProcessCwdOverride = ${JSON.stringify(processConfig.cwd)};`);
		parts.push(getIsolateRuntimeSource("overrideProcessCwd"));
	}

	// CJS file globals (__filename, __dirname, module) only for exec mode.
	if (mode === "exec") {
		const commonJsFileConfig = (() => {
			if (filePath) {
				const dirname = filePath.includes("/")
					? filePath.substring(0, filePath.lastIndexOf("/")) || "/"
					: "/";
				return { filePath, dirname };
			}
			if (processConfig.cwd) {
				return {
					filePath: `${processConfig.cwd.replace(/\/$/, "") || "/"}/[eval].js`,
					dirname: processConfig.cwd,
				};
			}
			return null;
		})();
		if (bridgeConfig.stdin !== undefined) {
			parts.push(`globalThis.__runtimeStdinData = ${JSON.stringify(bridgeConfig.stdin)};`);
			parts.push(getIsolateRuntimeSource("setStdinData"));
		}
		// Set CommonJS globals
		parts.push(getIsolateRuntimeSource("initCommonjsModuleGlobals"));
		if (commonJsFileConfig) {
			parts.push(`globalThis.__runtimeCommonJsFileConfig = ${JSON.stringify(commonJsFileConfig)};`);
			parts.push(getIsolateRuntimeSource("setCommonjsFileGlobals"));
		}
	} else {
		// run mode — still need CommonJS module globals
		parts.push(getIsolateRuntimeSource("initCommonjsModuleGlobals"));
		if (filePath) {
			const dirname = filePath.includes("/")
				? filePath.substring(0, filePath.lastIndexOf("/")) || "/"
				: "/";
			parts.push(`globalThis.__runtimeCommonJsFileConfig = ${JSON.stringify({ filePath, dirname })};`);
			parts.push(getIsolateRuntimeSource("setCommonjsFileGlobals"));
		}
	}

	// Apply custom global exposure policy
	parts.push(`globalThis.__runtimeCustomGlobalPolicy = ${JSON.stringify({
		hardenedGlobals: getHardenedGlobals(),
		mutableGlobals: getMutableGlobals(),
	})};`);
	parts.push(getIsolateRuntimeSource("applyCustomGlobalPolicy"));

	// Inflate SecureExec.bindings from flattened __bind.* globals
	parts.push(buildBindingsInflationSnippet(bindingKeys ?? []));

	return parts.join("\n");
}

// Import global exposure policy constants
import {
	HARDENED_NODE_CUSTOM_GLOBALS,
	MUTABLE_NODE_CUSTOM_GLOBALS,
} from "@secure-exec/core/internal/shared/global-exposure";
import {
	HOST_BRIDGE_GLOBAL_KEYS,
} from "./bridge-contract.js";

function getHardenedGlobals(): string[] { return HARDENED_NODE_CUSTOM_GLOBALS; }
function getMutableGlobals(): string[] { return MUTABLE_NODE_CUSTOM_GLOBALS; }
function getProcessConfigGlobalKey(): string { return HOST_BRIDGE_GLOBAL_KEYS.processConfig; }
function getOsConfigGlobalKey(): string { return HOST_BRIDGE_GLOBAL_KEYS.osConfig; }

/** Build the JS snippet that inflates __bind.* globals into a frozen SecureExec.bindings tree. */
function buildBindingsInflationSnippet(bindingKeys: string[]): string {
	// Build dispatch wrappers for each binding key and assign directly to the
	// tree nodes. Uses _loadPolyfill as the dispatch multiplexer (same as the
	// static dispatch shim for internal bridge globals).
	return `(function(){
var __bindingKeys__=${JSON.stringify(bindingKeys)};
var tree={};
function makeBindFn(bk){
return function(){var args=Array.prototype.slice.call(arguments);var encoded="__bd:"+bk+":"+JSON.stringify(args);var r=_loadPolyfill.applySyncPromise(undefined,[encoded]);if(r===null)return undefined;try{var p=JSON.parse(r);if(p.__bd_error)throw new Error(p.__bd_error);return p.__bd_result;}catch(e){if(e.message&&e.message.startsWith("No handler:"))return undefined;throw e;}};
}
for(var i=0;i<__bindingKeys__.length;i++){
var parts=__bindingKeys__[i].split(".");
var node=tree;
for(var j=0;j<parts.length-1;j++){node[parts[j]]=node[parts[j]]||{};node=node[parts[j]];}
node[parts[parts.length-1]]=makeBindFn("__bind."+__bindingKeys__[i]);
}
function deepFreeze(obj){
var vals=Object.values(obj);
for(var k=0;k<vals.length;k++){if(typeof vals[k]==="object"&&vals[k]!==null)deepFreeze(vals[k]);}
return Object.freeze(obj);
}
Object.defineProperty(globalThis,"SecureExec",{value:Object.freeze({bindings:deepFreeze(tree)}),writable:false,enumerable:true,configurable:false});
})();`;
}
