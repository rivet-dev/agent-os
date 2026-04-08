import * as bufferStdlibModuleNs from "node:buffer";
import * as constantsStdlibModuleNs from "node:constants";
import * as eventsStdlibModuleNs from "node:events";
import * as pathStdlibModuleNs from "node:path";
import * as punycodeStdlibModuleNs from "node:punycode";
import * as querystringStdlibModuleNs from "node:querystring";
import * as streamStdlibModuleNs from "node:stream";
import * as stringDecoderStdlibModuleNs from "node:string_decoder";
import * as urlStdlibModuleNs from "node:url";
import * as utilStdlibModuleNs from "node:util";
import {
  WebReadableStream,
  WebWritableStream,
  WebTransformStream,
} from "./undici-shims/web-streams-global.js";
import undiciAgentModule from "undici/lib/dispatcher/agent.js";
import undiciFetchModule from "undici/lib/web/fetch/index.js";
import undiciGlobalModule from "undici/lib/global.js";
import undiciHeadersModule from "undici/lib/web/fetch/headers.js";
import undiciRequestModule from "undici/lib/web/fetch/request.js";
import undiciResponseModule from "undici/lib/web/fetch/response.js";

const EarlyBufferGlobal =
  bufferStdlibModuleNs.Buffer ??
  bufferStdlibModuleNs.default?.Buffer ??
  bufferStdlibModuleNs.default;
if (typeof globalThis.Buffer === "undefined" && typeof EarlyBufferGlobal === "function") {
  globalThis.Buffer = EarlyBufferGlobal;
}

const EarlyUtilTypes =
  utilStdlibModuleNs.types ??
  utilStdlibModuleNs.default?.types;
if (EarlyUtilTypes && typeof EarlyUtilTypes.isProxy !== "function") {
  EarlyUtilTypes.isProxy = () => false;
} else if (EarlyUtilTypes) {
  try {
    EarlyUtilTypes.isProxy({});
  } catch (_error) {
    EarlyUtilTypes.isProxy = () => false;
  }
}

"use strict";
var __bridge = (() => {
  var __create = Object.create;
  var __defProp = Object.defineProperty;
  var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
  var __getOwnPropNames = Object.getOwnPropertyNames;
  var __getProtoOf = Object.getPrototypeOf;
  var __hasOwnProp = Object.prototype.hasOwnProperty;
  var __commonJS = (cb, mod) => function __require() {
    return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
  };
  var __export = (target, all) => {
    for (var name in all)
      __defProp(target, name, { get: all[name], enumerable: true });
  };
  var __copyProps = (to, from, except, desc) => {
    if (from && typeof from === "object" || typeof from === "function") {
      for (let key of __getOwnPropNames(from))
        if (!__hasOwnProp.call(to, key) && key !== except)
          __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
    }
    return to;
  };
  var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
    // If the importer is in node compatibility mode or this is not an ESM
    // file that has been converted to a CommonJS file using a Babel-
    // compatible transform (i.e. "__esModule" has not been set), then set
    // "default" to the CommonJS "module.exports" for node compatibility.
    isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
    mod
  ));
  var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

  // ../../../tmp/buffer-build/node_modules/base64-js/index.js
  var require_base64_js = __commonJS({
    "../../../tmp/buffer-build/node_modules/base64-js/index.js"(exports) {
      "use strict";
      exports.byteLength = byteLength;
      exports.toByteArray = toByteArray;
      exports.fromByteArray = fromByteArray;
      var lookup = [];
      var revLookup = [];
      var Arr = typeof Uint8Array !== "undefined" ? Uint8Array : Array;
      var code = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
      for (i = 0, len = code.length; i < len; ++i) {
        lookup[i] = code[i];
        revLookup[code.charCodeAt(i)] = i;
      }
      var i;
      var len;
      revLookup["-".charCodeAt(0)] = 62;
      revLookup["_".charCodeAt(0)] = 63;
      function getLens(b64) {
        var len2 = b64.length;
        if (len2 % 4 > 0) {
          throw new Error("Invalid string. Length must be a multiple of 4");
        }
        var validLen = b64.indexOf("=");
        if (validLen === -1) validLen = len2;
        var placeHoldersLen = validLen === len2 ? 0 : 4 - validLen % 4;
        return [validLen, placeHoldersLen];
      }
      function byteLength(b64) {
        var lens = getLens(b64);
        var validLen = lens[0];
        var placeHoldersLen = lens[1];
        return (validLen + placeHoldersLen) * 3 / 4 - placeHoldersLen;
      }
      function _byteLength(b64, validLen, placeHoldersLen) {
        return (validLen + placeHoldersLen) * 3 / 4 - placeHoldersLen;
      }
      function toByteArray(b64) {
        var tmp;
        var lens = getLens(b64);
        var validLen = lens[0];
        var placeHoldersLen = lens[1];
        var arr = new Arr(_byteLength(b64, validLen, placeHoldersLen));
        var curByte = 0;
        var len2 = placeHoldersLen > 0 ? validLen - 4 : validLen;
        var i2;
        for (i2 = 0; i2 < len2; i2 += 4) {
          tmp = revLookup[b64.charCodeAt(i2)] << 18 | revLookup[b64.charCodeAt(i2 + 1)] << 12 | revLookup[b64.charCodeAt(i2 + 2)] << 6 | revLookup[b64.charCodeAt(i2 + 3)];
          arr[curByte++] = tmp >> 16 & 255;
          arr[curByte++] = tmp >> 8 & 255;
          arr[curByte++] = tmp & 255;
        }
        if (placeHoldersLen === 2) {
          tmp = revLookup[b64.charCodeAt(i2)] << 2 | revLookup[b64.charCodeAt(i2 + 1)] >> 4;
          arr[curByte++] = tmp & 255;
        }
        if (placeHoldersLen === 1) {
          tmp = revLookup[b64.charCodeAt(i2)] << 10 | revLookup[b64.charCodeAt(i2 + 1)] << 4 | revLookup[b64.charCodeAt(i2 + 2)] >> 2;
          arr[curByte++] = tmp >> 8 & 255;
          arr[curByte++] = tmp & 255;
        }
        return arr;
      }
      function tripletToBase64(num) {
        return lookup[num >> 18 & 63] + lookup[num >> 12 & 63] + lookup[num >> 6 & 63] + lookup[num & 63];
      }
      function encodeChunk(uint8, start, end) {
        var tmp;
        var output = [];
        for (var i2 = start; i2 < end; i2 += 3) {
          tmp = (uint8[i2] << 16 & 16711680) + (uint8[i2 + 1] << 8 & 65280) + (uint8[i2 + 2] & 255);
          output.push(tripletToBase64(tmp));
        }
        return output.join("");
      }
      function fromByteArray(uint8) {
        var tmp;
        var len2 = uint8.length;
        var extraBytes = len2 % 3;
        var parts = [];
        var maxChunkLength = 16383;
        for (var i2 = 0, len22 = len2 - extraBytes; i2 < len22; i2 += maxChunkLength) {
          parts.push(encodeChunk(uint8, i2, i2 + maxChunkLength > len22 ? len22 : i2 + maxChunkLength));
        }
        if (extraBytes === 1) {
          tmp = uint8[len2 - 1];
          parts.push(
            lookup[tmp >> 2] + lookup[tmp << 4 & 63] + "=="
          );
        } else if (extraBytes === 2) {
          tmp = (uint8[len2 - 2] << 8) + uint8[len2 - 1];
          parts.push(
            lookup[tmp >> 10] + lookup[tmp >> 4 & 63] + lookup[tmp << 2 & 63] + "="
          );
        }
        return parts.join("");
      }
    }
  });

  // ../../../tmp/buffer-build/node_modules/ieee754/index.js
  var require_ieee754 = __commonJS({
    "../../../tmp/buffer-build/node_modules/ieee754/index.js"(exports) {
      exports.read = function(buffer, offset, isLE, mLen, nBytes) {
        var e, m;
        var eLen = nBytes * 8 - mLen - 1;
        var eMax = (1 << eLen) - 1;
        var eBias = eMax >> 1;
        var nBits = -7;
        var i = isLE ? nBytes - 1 : 0;
        var d = isLE ? -1 : 1;
        var s = buffer[offset + i];
        i += d;
        e = s & (1 << -nBits) - 1;
        s >>= -nBits;
        nBits += eLen;
        for (; nBits > 0; e = e * 256 + buffer[offset + i], i += d, nBits -= 8) {
        }
        m = e & (1 << -nBits) - 1;
        e >>= -nBits;
        nBits += mLen;
        for (; nBits > 0; m = m * 256 + buffer[offset + i], i += d, nBits -= 8) {
        }
        if (e === 0) {
          e = 1 - eBias;
        } else if (e === eMax) {
          return m ? NaN : (s ? -1 : 1) * Infinity;
        } else {
          m = m + Math.pow(2, mLen);
          e = e - eBias;
        }
        return (s ? -1 : 1) * m * Math.pow(2, e - mLen);
      };
      exports.write = function(buffer, value, offset, isLE, mLen, nBytes) {
        var e, m, c;
        var eLen = nBytes * 8 - mLen - 1;
        var eMax = (1 << eLen) - 1;
        var eBias = eMax >> 1;
        var rt = mLen === 23 ? Math.pow(2, -24) - Math.pow(2, -77) : 0;
        var i = isLE ? 0 : nBytes - 1;
        var d = isLE ? 1 : -1;
        var s = value < 0 || value === 0 && 1 / value < 0 ? 1 : 0;
        value = Math.abs(value);
        if (isNaN(value) || value === Infinity) {
          m = isNaN(value) ? 1 : 0;
          e = eMax;
        } else {
          e = Math.floor(Math.log(value) / Math.LN2);
          if (value * (c = Math.pow(2, -e)) < 1) {
            e--;
            c *= 2;
          }
          if (e + eBias >= 1) {
            value += rt / c;
          } else {
            value += rt * Math.pow(2, 1 - eBias);
          }
          if (value * c >= 2) {
            e++;
            c /= 2;
          }
          if (e + eBias >= eMax) {
            m = 0;
            e = eMax;
          } else if (e + eBias >= 1) {
            m = (value * c - 1) * Math.pow(2, mLen);
            e = e + eBias;
          } else {
            m = value * Math.pow(2, eBias - 1) * Math.pow(2, mLen);
            e = 0;
          }
        }
        for (; mLen >= 8; buffer[offset + i] = m & 255, i += d, m /= 256, mLen -= 8) {
        }
        e = e << mLen | m;
        eLen += mLen;
        for (; eLen > 0; buffer[offset + i] = e & 255, i += d, e /= 256, eLen -= 8) {
        }
        buffer[offset + i - d] |= s * 128;
      };
    }
  });

  // ../../../tmp/buffer-build/node_modules/buffer/index.js
  var require_buffer = __commonJS({
    "../../../tmp/buffer-build/node_modules/buffer/index.js"(exports) {
      "use strict";
      var base64 = require_base64_js();
      var ieee754 = require_ieee754();
      var customInspectSymbol = typeof Symbol === "function" && typeof Symbol["for"] === "function" ? Symbol["for"]("nodejs.util.inspect.custom") : null;
      exports.Buffer = Buffer4;
      exports.SlowBuffer = SlowBuffer;
      exports.INSPECT_MAX_BYTES = 50;
      var K_MAX_LENGTH = 2147483647;
      exports.kMaxLength = K_MAX_LENGTH;
      Buffer4.TYPED_ARRAY_SUPPORT = typedArraySupport();
      if (!Buffer4.TYPED_ARRAY_SUPPORT && typeof console !== "undefined" && typeof console.error === "function") {
        console.error(
          "This browser lacks typed array (Uint8Array) support which is required by `buffer` v5.x. Use `buffer` v4.x if you require old browser support."
        );
      }
      function typedArraySupport() {
        try {
          const arr = new Uint8Array(1);
          const proto = { foo: function() {
            return 42;
          } };
          Object.setPrototypeOf(proto, Uint8Array.prototype);
          Object.setPrototypeOf(arr, proto);
          return arr.foo() === 42;
        } catch (e) {
          return false;
        }
      }
      Object.defineProperty(Buffer4.prototype, "parent", {
        enumerable: true,
        get: function() {
          if (!Buffer4.isBuffer(this)) return void 0;
          return this.buffer;
        }
      });
      Object.defineProperty(Buffer4.prototype, "offset", {
        enumerable: true,
        get: function() {
          if (!Buffer4.isBuffer(this)) return void 0;
          return this.byteOffset;
        }
      });
      function createBuffer(length) {
        if (length > K_MAX_LENGTH) {
          throw new RangeError('The value "' + length + '" is invalid for option "size"');
        }
        const buf = new Uint8Array(length);
        Object.setPrototypeOf(buf, Buffer4.prototype);
        return buf;
      }
      function Buffer4(arg, encodingOrOffset, length) {
        if (typeof arg === "number") {
          if (typeof encodingOrOffset === "string") {
            throw new TypeError(
              'The "string" argument must be of type string. Received type number'
            );
          }
          return allocUnsafe(arg);
        }
        return from(arg, encodingOrOffset, length);
      }
      Buffer4.poolSize = 8192;
      function from(value, encodingOrOffset, length) {
        if (typeof value === "string") {
          return fromString(value, encodingOrOffset);
        }
        if (ArrayBuffer.isView(value)) {
          return fromArrayView(value);
        }
        if (value == null) {
          throw new TypeError(
            "The first argument must be one of type string, Buffer, ArrayBuffer, Array, or Array-like Object. Received type " + typeof value
          );
        }
        if (isInstance(value, ArrayBuffer) || value && isInstance(value.buffer, ArrayBuffer)) {
          return fromArrayBuffer(value, encodingOrOffset, length);
        }
        if (typeof SharedArrayBuffer !== "undefined" && (isInstance(value, SharedArrayBuffer) || value && isInstance(value.buffer, SharedArrayBuffer))) {
          return fromArrayBuffer(value, encodingOrOffset, length);
        }
        if (typeof value === "number") {
          throw new TypeError(
            'The "value" argument must not be of type number. Received type number'
          );
        }
        const valueOf = value.valueOf && value.valueOf();
        if (valueOf != null && valueOf !== value) {
          return Buffer4.from(valueOf, encodingOrOffset, length);
        }
        const b = fromObject(value);
        if (b) return b;
        if (typeof Symbol !== "undefined" && Symbol.toPrimitive != null && typeof value[Symbol.toPrimitive] === "function") {
          return Buffer4.from(value[Symbol.toPrimitive]("string"), encodingOrOffset, length);
        }
        throw new TypeError(
          "The first argument must be one of type string, Buffer, ArrayBuffer, Array, or Array-like Object. Received type " + typeof value
        );
      }
      Buffer4.from = function(value, encodingOrOffset, length) {
        return from(value, encodingOrOffset, length);
      };
      Object.setPrototypeOf(Buffer4.prototype, Uint8Array.prototype);
      Object.setPrototypeOf(Buffer4, Uint8Array);
      function assertSize(size) {
        if (typeof size !== "number") {
          throw new TypeError('"size" argument must be of type number');
        } else if (size < 0) {
          throw new RangeError('The value "' + size + '" is invalid for option "size"');
        }
      }
      function alloc(size, fill, encoding) {
        assertSize(size);
        if (size <= 0) {
          return createBuffer(size);
        }
        if (fill !== void 0) {
          return typeof encoding === "string" ? createBuffer(size).fill(fill, encoding) : createBuffer(size).fill(fill);
        }
        return createBuffer(size);
      }
      Buffer4.alloc = function(size, fill, encoding) {
        return alloc(size, fill, encoding);
      };
      function allocUnsafe(size) {
        assertSize(size);
        return createBuffer(size < 0 ? 0 : checked(size) | 0);
      }
      Buffer4.allocUnsafe = function(size) {
        return allocUnsafe(size);
      };
      Buffer4.allocUnsafeSlow = function(size) {
        return allocUnsafe(size);
      };
      function fromString(string, encoding) {
        if (typeof encoding !== "string" || encoding === "") {
          encoding = "utf8";
        }
        if (!Buffer4.isEncoding(encoding)) {
          throw new TypeError("Unknown encoding: " + encoding);
        }
        const length = byteLength(string, encoding) | 0;
        let buf = createBuffer(length);
        const actual = buf.write(string, encoding);
        if (actual !== length) {
          buf = buf.slice(0, actual);
        }
        return buf;
      }
      function fromArrayLike(array) {
        const length = array.length < 0 ? 0 : checked(array.length) | 0;
        const buf = createBuffer(length);
        for (let i = 0; i < length; i += 1) {
          buf[i] = array[i] & 255;
        }
        return buf;
      }
      function fromArrayView(arrayView) {
        if (isInstance(arrayView, Uint8Array)) {
          const copy = new Uint8Array(arrayView);
          return fromArrayBuffer(copy.buffer, copy.byteOffset, copy.byteLength);
        }
        return fromArrayLike(arrayView);
      }
      function fromArrayBuffer(array, byteOffset, length) {
        if (byteOffset < 0 || array.byteLength < byteOffset) {
          throw new RangeError('"offset" is outside of buffer bounds');
        }
        if (array.byteLength < byteOffset + (length || 0)) {
          throw new RangeError('"length" is outside of buffer bounds');
        }
        let buf;
        if (byteOffset === void 0 && length === void 0) {
          buf = new Uint8Array(array);
        } else if (length === void 0) {
          buf = new Uint8Array(array, byteOffset);
        } else {
          buf = new Uint8Array(array, byteOffset, length);
        }
        Object.setPrototypeOf(buf, Buffer4.prototype);
        return buf;
      }
      function fromObject(obj) {
        if (Buffer4.isBuffer(obj)) {
          const len = checked(obj.length) | 0;
          const buf = createBuffer(len);
          if (buf.length === 0) {
            return buf;
          }
          obj.copy(buf, 0, 0, len);
          return buf;
        }
        if (obj.length !== void 0) {
          if (typeof obj.length !== "number" || numberIsNaN(obj.length)) {
            return createBuffer(0);
          }
          return fromArrayLike(obj);
        }
        if (obj.type === "Buffer" && Array.isArray(obj.data)) {
          return fromArrayLike(obj.data);
        }
      }
      function checked(length) {
        if (length >= K_MAX_LENGTH) {
          throw new RangeError("Attempt to allocate Buffer larger than maximum size: 0x" + K_MAX_LENGTH.toString(16) + " bytes");
        }
        return length | 0;
      }
      function SlowBuffer(length) {
        if (+length != length) {
          length = 0;
        }
        return Buffer4.alloc(+length);
      }
      Buffer4.isBuffer = function isBuffer(b) {
        return b != null && b._isBuffer === true && b !== Buffer4.prototype;
      };
      Buffer4.compare = function compare(a, b) {
        if (isInstance(a, Uint8Array)) a = Buffer4.from(a, a.offset, a.byteLength);
        if (isInstance(b, Uint8Array)) b = Buffer4.from(b, b.offset, b.byteLength);
        if (!Buffer4.isBuffer(a) || !Buffer4.isBuffer(b)) {
          throw new TypeError(
            'The "buf1", "buf2" arguments must be one of type Buffer or Uint8Array'
          );
        }
        if (a === b) return 0;
        let x = a.length;
        let y = b.length;
        for (let i = 0, len = Math.min(x, y); i < len; ++i) {
          if (a[i] !== b[i]) {
            x = a[i];
            y = b[i];
            break;
          }
        }
        if (x < y) return -1;
        if (y < x) return 1;
        return 0;
      };
      Buffer4.isEncoding = function isEncoding(encoding) {
        switch (String(encoding).toLowerCase()) {
          case "hex":
          case "utf8":
          case "utf-8":
          case "ascii":
          case "latin1":
          case "binary":
          case "base64":
          case "ucs2":
          case "ucs-2":
          case "utf16le":
          case "utf-16le":
            return true;
          default:
            return false;
        }
      };
      Buffer4.concat = function concat(list, length) {
        if (!Array.isArray(list)) {
          throw new TypeError('"list" argument must be an Array of Buffers');
        }
        if (list.length === 0) {
          return Buffer4.alloc(0);
        }
        let i;
        if (length === void 0) {
          length = 0;
          for (i = 0; i < list.length; ++i) {
            length += list[i].length;
          }
        }
        const buffer = Buffer4.allocUnsafe(length);
        let pos = 0;
        for (i = 0; i < list.length; ++i) {
          let buf = list[i];
          if (isInstance(buf, Uint8Array)) {
            if (pos + buf.length > buffer.length) {
              if (!Buffer4.isBuffer(buf)) buf = Buffer4.from(buf);
              buf.copy(buffer, pos);
            } else {
              Uint8Array.prototype.set.call(
                buffer,
                buf,
                pos
              );
            }
          } else if (!Buffer4.isBuffer(buf)) {
            throw new TypeError('"list" argument must be an Array of Buffers');
          } else {
            buf.copy(buffer, pos);
          }
          pos += buf.length;
        }
        return buffer;
      };
      function byteLength(string, encoding) {
        if (Buffer4.isBuffer(string)) {
          return string.length;
        }
        if (ArrayBuffer.isView(string) || isInstance(string, ArrayBuffer)) {
          return string.byteLength;
        }
        if (typeof string !== "string") {
          throw new TypeError(
            'The "string" argument must be one of type string, Buffer, or ArrayBuffer. Received type ' + typeof string
          );
        }
        const len = string.length;
        const mustMatch = arguments.length > 2 && arguments[2] === true;
        if (!mustMatch && len === 0) return 0;
        let loweredCase = false;
        for (; ; ) {
          switch (encoding) {
            case "ascii":
            case "latin1":
            case "binary":
              return len;
            case "utf8":
            case "utf-8":
              return utf8ToBytes(string).length;
            case "ucs2":
            case "ucs-2":
            case "utf16le":
            case "utf-16le":
              return len * 2;
            case "hex":
              return len >>> 1;
            case "base64":
              return base64ToBytes(string).length;
            default:
              if (loweredCase) {
                return mustMatch ? -1 : utf8ToBytes(string).length;
              }
              encoding = ("" + encoding).toLowerCase();
              loweredCase = true;
          }
        }
      }
      Buffer4.byteLength = byteLength;
      function slowToString(encoding, start, end) {
        let loweredCase = false;
        if (start === void 0 || start < 0) {
          start = 0;
        }
        if (start > this.length) {
          return "";
        }
        if (end === void 0 || end > this.length) {
          end = this.length;
        }
        if (end <= 0) {
          return "";
        }
        end >>>= 0;
        start >>>= 0;
        if (end <= start) {
          return "";
        }
        if (!encoding) encoding = "utf8";
        while (true) {
          switch (encoding) {
            case "hex":
              return hexSlice(this, start, end);
            case "utf8":
            case "utf-8":
              return utf8Slice(this, start, end);
            case "ascii":
              return asciiSlice(this, start, end);
            case "latin1":
            case "binary":
              return latin1Slice(this, start, end);
            case "base64":
              return base64Slice(this, start, end);
            case "ucs2":
            case "ucs-2":
            case "utf16le":
            case "utf-16le":
              return utf16leSlice(this, start, end);
            default:
              if (loweredCase) throw new TypeError("Unknown encoding: " + encoding);
              encoding = (encoding + "").toLowerCase();
              loweredCase = true;
          }
        }
      }
      Buffer4.prototype._isBuffer = true;
      function swap(b, n, m) {
        const i = b[n];
        b[n] = b[m];
        b[m] = i;
      }
      Buffer4.prototype.swap16 = function swap16() {
        const len = this.length;
        if (len % 2 !== 0) {
          throw new RangeError("Buffer size must be a multiple of 16-bits");
        }
        for (let i = 0; i < len; i += 2) {
          swap(this, i, i + 1);
        }
        return this;
      };
      Buffer4.prototype.swap32 = function swap32() {
        const len = this.length;
        if (len % 4 !== 0) {
          throw new RangeError("Buffer size must be a multiple of 32-bits");
        }
        for (let i = 0; i < len; i += 4) {
          swap(this, i, i + 3);
          swap(this, i + 1, i + 2);
        }
        return this;
      };
      Buffer4.prototype.swap64 = function swap64() {
        const len = this.length;
        if (len % 8 !== 0) {
          throw new RangeError("Buffer size must be a multiple of 64-bits");
        }
        for (let i = 0; i < len; i += 8) {
          swap(this, i, i + 7);
          swap(this, i + 1, i + 6);
          swap(this, i + 2, i + 5);
          swap(this, i + 3, i + 4);
        }
        return this;
      };
      Buffer4.prototype.toString = function toString() {
        const length = this.length;
        if (length === 0) return "";
        if (arguments.length === 0) return utf8Slice(this, 0, length);
        return slowToString.apply(this, arguments);
      };
      Buffer4.prototype.toLocaleString = Buffer4.prototype.toString;
      Buffer4.prototype.equals = function equals(b) {
        if (!Buffer4.isBuffer(b)) throw new TypeError("Argument must be a Buffer");
        if (this === b) return true;
        return Buffer4.compare(this, b) === 0;
      };
      Buffer4.prototype.inspect = function inspect() {
        let str = "";
        const max = exports.INSPECT_MAX_BYTES;
        str = this.toString("hex", 0, max).replace(/(.{2})/g, "$1 ").trim();
        if (this.length > max) str += " ... ";
        return "<Buffer " + str + ">";
      };
      if (customInspectSymbol) {
        Buffer4.prototype[customInspectSymbol] = Buffer4.prototype.inspect;
      }
      Buffer4.prototype.compare = function compare(target, start, end, thisStart, thisEnd) {
        if (isInstance(target, Uint8Array)) {
          target = Buffer4.from(target, target.offset, target.byteLength);
        }
        if (!Buffer4.isBuffer(target)) {
          throw new TypeError(
            'The "target" argument must be one of type Buffer or Uint8Array. Received type ' + typeof target
          );
        }
        if (start === void 0) {
          start = 0;
        }
        if (end === void 0) {
          end = target ? target.length : 0;
        }
        if (thisStart === void 0) {
          thisStart = 0;
        }
        if (thisEnd === void 0) {
          thisEnd = this.length;
        }
        if (start < 0 || end > target.length || thisStart < 0 || thisEnd > this.length) {
          throw new RangeError("out of range index");
        }
        if (thisStart >= thisEnd && start >= end) {
          return 0;
        }
        if (thisStart >= thisEnd) {
          return -1;
        }
        if (start >= end) {
          return 1;
        }
        start >>>= 0;
        end >>>= 0;
        thisStart >>>= 0;
        thisEnd >>>= 0;
        if (this === target) return 0;
        let x = thisEnd - thisStart;
        let y = end - start;
        const len = Math.min(x, y);
        const thisCopy = this.slice(thisStart, thisEnd);
        const targetCopy = target.slice(start, end);
        for (let i = 0; i < len; ++i) {
          if (thisCopy[i] !== targetCopy[i]) {
            x = thisCopy[i];
            y = targetCopy[i];
            break;
          }
        }
        if (x < y) return -1;
        if (y < x) return 1;
        return 0;
      };
      function bidirectionalIndexOf(buffer, val, byteOffset, encoding, dir) {
        if (buffer.length === 0) return -1;
        if (typeof byteOffset === "string") {
          encoding = byteOffset;
          byteOffset = 0;
        } else if (byteOffset > 2147483647) {
          byteOffset = 2147483647;
        } else if (byteOffset < -2147483648) {
          byteOffset = -2147483648;
        }
        byteOffset = +byteOffset;
        if (numberIsNaN(byteOffset)) {
          byteOffset = dir ? 0 : buffer.length - 1;
        }
        if (byteOffset < 0) byteOffset = buffer.length + byteOffset;
        if (byteOffset >= buffer.length) {
          if (dir) return -1;
          else byteOffset = buffer.length - 1;
        } else if (byteOffset < 0) {
          if (dir) byteOffset = 0;
          else return -1;
        }
        if (typeof val === "string") {
          val = Buffer4.from(val, encoding);
        }
        if (Buffer4.isBuffer(val)) {
          if (val.length === 0) {
            return -1;
          }
          return arrayIndexOf(buffer, val, byteOffset, encoding, dir);
        } else if (typeof val === "number") {
          val = val & 255;
          if (typeof Uint8Array.prototype.indexOf === "function") {
            if (dir) {
              return Uint8Array.prototype.indexOf.call(buffer, val, byteOffset);
            } else {
              return Uint8Array.prototype.lastIndexOf.call(buffer, val, byteOffset);
            }
          }
          return arrayIndexOf(buffer, [val], byteOffset, encoding, dir);
        }
        throw new TypeError("val must be string, number or Buffer");
      }
      function arrayIndexOf(arr, val, byteOffset, encoding, dir) {
        let indexSize = 1;
        let arrLength = arr.length;
        let valLength = val.length;
        if (encoding !== void 0) {
          encoding = String(encoding).toLowerCase();
          if (encoding === "ucs2" || encoding === "ucs-2" || encoding === "utf16le" || encoding === "utf-16le") {
            if (arr.length < 2 || val.length < 2) {
              return -1;
            }
            indexSize = 2;
            arrLength /= 2;
            valLength /= 2;
            byteOffset /= 2;
          }
        }
        function read(buf, i2) {
          if (indexSize === 1) {
            return buf[i2];
          } else {
            return buf.readUInt16BE(i2 * indexSize);
          }
        }
        let i;
        if (dir) {
          let foundIndex = -1;
          for (i = byteOffset; i < arrLength; i++) {
            if (read(arr, i) === read(val, foundIndex === -1 ? 0 : i - foundIndex)) {
              if (foundIndex === -1) foundIndex = i;
              if (i - foundIndex + 1 === valLength) return foundIndex * indexSize;
            } else {
              if (foundIndex !== -1) i -= i - foundIndex;
              foundIndex = -1;
            }
          }
        } else {
          if (byteOffset + valLength > arrLength) byteOffset = arrLength - valLength;
          for (i = byteOffset; i >= 0; i--) {
            let found = true;
            for (let j = 0; j < valLength; j++) {
              if (read(arr, i + j) !== read(val, j)) {
                found = false;
                break;
              }
            }
            if (found) return i;
          }
        }
        return -1;
      }
      Buffer4.prototype.includes = function includes(val, byteOffset, encoding) {
        return this.indexOf(val, byteOffset, encoding) !== -1;
      };
      Buffer4.prototype.indexOf = function indexOf(val, byteOffset, encoding) {
        return bidirectionalIndexOf(this, val, byteOffset, encoding, true);
      };
      Buffer4.prototype.lastIndexOf = function lastIndexOf(val, byteOffset, encoding) {
        return bidirectionalIndexOf(this, val, byteOffset, encoding, false);
      };
      function hexWrite(buf, string, offset, length) {
        offset = Number(offset) || 0;
        const remaining = buf.length - offset;
        if (!length) {
          length = remaining;
        } else {
          length = Number(length);
          if (length > remaining) {
            length = remaining;
          }
        }
        const strLen = string.length;
        if (length > strLen / 2) {
          length = strLen / 2;
        }
        let i;
        for (i = 0; i < length; ++i) {
          const parsed = parseInt(string.substr(i * 2, 2), 16);
          if (numberIsNaN(parsed)) return i;
          buf[offset + i] = parsed;
        }
        return i;
      }
      function utf8Write(buf, string, offset, length) {
        return blitBuffer(utf8ToBytes(string, buf.length - offset), buf, offset, length);
      }
      function asciiWrite(buf, string, offset, length) {
        return blitBuffer(asciiToBytes(string), buf, offset, length);
      }
      function base64Write(buf, string, offset, length) {
        return blitBuffer(base64ToBytes(string), buf, offset, length);
      }
      function ucs2Write(buf, string, offset, length) {
        return blitBuffer(utf16leToBytes(string, buf.length - offset), buf, offset, length);
      }
      Buffer4.prototype.write = function write(string, offset, length, encoding) {
        if (offset === void 0) {
          encoding = "utf8";
          length = this.length;
          offset = 0;
        } else if (length === void 0 && typeof offset === "string") {
          encoding = offset;
          length = this.length;
          offset = 0;
        } else if (isFinite(offset)) {
          offset = offset >>> 0;
          if (isFinite(length)) {
            length = length >>> 0;
            if (encoding === void 0) encoding = "utf8";
          } else {
            encoding = length;
            length = void 0;
          }
        } else {
          throw new Error(
            "Buffer.write(string, encoding, offset[, length]) is no longer supported"
          );
        }
        const remaining = this.length - offset;
        if (length === void 0 || length > remaining) length = remaining;
        if (string.length > 0 && (length < 0 || offset < 0) || offset > this.length) {
          throw new RangeError("Attempt to write outside buffer bounds");
        }
        if (!encoding) encoding = "utf8";
        let loweredCase = false;
        for (; ; ) {
          switch (encoding) {
            case "hex":
              return hexWrite(this, string, offset, length);
            case "utf8":
            case "utf-8":
              return utf8Write(this, string, offset, length);
            case "ascii":
            case "latin1":
            case "binary":
              return asciiWrite(this, string, offset, length);
            case "base64":
              return base64Write(this, string, offset, length);
            case "ucs2":
            case "ucs-2":
            case "utf16le":
            case "utf-16le":
              return ucs2Write(this, string, offset, length);
            default:
              if (loweredCase) throw new TypeError("Unknown encoding: " + encoding);
              encoding = ("" + encoding).toLowerCase();
              loweredCase = true;
          }
        }
      };
      Buffer4.prototype.toJSON = function toJSON() {
        return {
          type: "Buffer",
          data: Array.prototype.slice.call(this._arr || this, 0)
        };
      };
      function base64Slice(buf, start, end) {
        if (start === 0 && end === buf.length) {
          return base64.fromByteArray(buf);
        } else {
          return base64.fromByteArray(buf.slice(start, end));
        }
      }
      function utf8Slice(buf, start, end) {
        end = Math.min(buf.length, end);
        const res = [];
        let i = start;
        while (i < end) {
          const firstByte = buf[i];
          let codePoint = null;
          let bytesPerSequence = firstByte > 239 ? 4 : firstByte > 223 ? 3 : firstByte > 191 ? 2 : 1;
          if (i + bytesPerSequence <= end) {
            let secondByte, thirdByte, fourthByte, tempCodePoint;
            switch (bytesPerSequence) {
              case 1:
                if (firstByte < 128) {
                  codePoint = firstByte;
                }
                break;
              case 2:
                secondByte = buf[i + 1];
                if ((secondByte & 192) === 128) {
                  tempCodePoint = (firstByte & 31) << 6 | secondByte & 63;
                  if (tempCodePoint > 127) {
                    codePoint = tempCodePoint;
                  }
                }
                break;
              case 3:
                secondByte = buf[i + 1];
                thirdByte = buf[i + 2];
                if ((secondByte & 192) === 128 && (thirdByte & 192) === 128) {
                  tempCodePoint = (firstByte & 15) << 12 | (secondByte & 63) << 6 | thirdByte & 63;
                  if (tempCodePoint > 2047 && (tempCodePoint < 55296 || tempCodePoint > 57343)) {
                    codePoint = tempCodePoint;
                  }
                }
                break;
              case 4:
                secondByte = buf[i + 1];
                thirdByte = buf[i + 2];
                fourthByte = buf[i + 3];
                if ((secondByte & 192) === 128 && (thirdByte & 192) === 128 && (fourthByte & 192) === 128) {
                  tempCodePoint = (firstByte & 15) << 18 | (secondByte & 63) << 12 | (thirdByte & 63) << 6 | fourthByte & 63;
                  if (tempCodePoint > 65535 && tempCodePoint < 1114112) {
                    codePoint = tempCodePoint;
                  }
                }
            }
          }
          if (codePoint === null) {
            codePoint = 65533;
            bytesPerSequence = 1;
          } else if (codePoint > 65535) {
            codePoint -= 65536;
            res.push(codePoint >>> 10 & 1023 | 55296);
            codePoint = 56320 | codePoint & 1023;
          }
          res.push(codePoint);
          i += bytesPerSequence;
        }
        return decodeCodePointsArray(res);
      }
      var MAX_ARGUMENTS_LENGTH = 4096;
      function decodeCodePointsArray(codePoints) {
        const len = codePoints.length;
        if (len <= MAX_ARGUMENTS_LENGTH) {
          return String.fromCharCode.apply(String, codePoints);
        }
        let res = "";
        let i = 0;
        while (i < len) {
          res += String.fromCharCode.apply(
            String,
            codePoints.slice(i, i += MAX_ARGUMENTS_LENGTH)
          );
        }
        return res;
      }
      function asciiSlice(buf, start, end) {
        let ret = "";
        end = Math.min(buf.length, end);
        for (let i = start; i < end; ++i) {
          ret += String.fromCharCode(buf[i] & 127);
        }
        return ret;
      }
      function latin1Slice(buf, start, end) {
        let ret = "";
        end = Math.min(buf.length, end);
        for (let i = start; i < end; ++i) {
          ret += String.fromCharCode(buf[i]);
        }
        return ret;
      }
      function hexSlice(buf, start, end) {
        const len = buf.length;
        if (!start || start < 0) start = 0;
        if (!end || end < 0 || end > len) end = len;
        let out = "";
        for (let i = start; i < end; ++i) {
          out += hexSliceLookupTable[buf[i]];
        }
        return out;
      }
      function utf16leSlice(buf, start, end) {
        const bytes = buf.slice(start, end);
        let res = "";
        for (let i = 0; i < bytes.length - 1; i += 2) {
          res += String.fromCharCode(bytes[i] + bytes[i + 1] * 256);
        }
        return res;
      }
      Buffer4.prototype.slice = function slice(start, end) {
        const len = this.length;
        start = ~~start;
        end = end === void 0 ? len : ~~end;
        if (start < 0) {
          start += len;
          if (start < 0) start = 0;
        } else if (start > len) {
          start = len;
        }
        if (end < 0) {
          end += len;
          if (end < 0) end = 0;
        } else if (end > len) {
          end = len;
        }
        if (end < start) end = start;
        const newBuf = this.subarray(start, end);
        Object.setPrototypeOf(newBuf, Buffer4.prototype);
        return newBuf;
      };
      function checkOffset(offset, ext, length) {
        if (offset % 1 !== 0 || offset < 0) throw new RangeError("offset is not uint");
        if (offset + ext > length) throw new RangeError("Trying to access beyond buffer length");
      }
      Buffer4.prototype.readUintLE = Buffer4.prototype.readUIntLE = function readUIntLE(offset, byteLength2, noAssert) {
        offset = offset >>> 0;
        byteLength2 = byteLength2 >>> 0;
        if (!noAssert) checkOffset(offset, byteLength2, this.length);
        let val = this[offset];
        let mul = 1;
        let i = 0;
        while (++i < byteLength2 && (mul *= 256)) {
          val += this[offset + i] * mul;
        }
        return val;
      };
      Buffer4.prototype.readUintBE = Buffer4.prototype.readUIntBE = function readUIntBE(offset, byteLength2, noAssert) {
        offset = offset >>> 0;
        byteLength2 = byteLength2 >>> 0;
        if (!noAssert) {
          checkOffset(offset, byteLength2, this.length);
        }
        let val = this[offset + --byteLength2];
        let mul = 1;
        while (byteLength2 > 0 && (mul *= 256)) {
          val += this[offset + --byteLength2] * mul;
        }
        return val;
      };
      Buffer4.prototype.readUint8 = Buffer4.prototype.readUInt8 = function readUInt8(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 1, this.length);
        return this[offset];
      };
      Buffer4.prototype.readUint16LE = Buffer4.prototype.readUInt16LE = function readUInt16LE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 2, this.length);
        return this[offset] | this[offset + 1] << 8;
      };
      Buffer4.prototype.readUint16BE = Buffer4.prototype.readUInt16BE = function readUInt16BE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 2, this.length);
        return this[offset] << 8 | this[offset + 1];
      };
      Buffer4.prototype.readUint32LE = Buffer4.prototype.readUInt32LE = function readUInt32LE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 4, this.length);
        return (this[offset] | this[offset + 1] << 8 | this[offset + 2] << 16) + this[offset + 3] * 16777216;
      };
      Buffer4.prototype.readUint32BE = Buffer4.prototype.readUInt32BE = function readUInt32BE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 4, this.length);
        return this[offset] * 16777216 + (this[offset + 1] << 16 | this[offset + 2] << 8 | this[offset + 3]);
      };
      Buffer4.prototype.readBigUInt64LE = defineBigIntMethod(function readBigUInt64LE(offset) {
        offset = offset >>> 0;
        validateNumber(offset, "offset");
        const first = this[offset];
        const last = this[offset + 7];
        if (first === void 0 || last === void 0) {
          boundsError(offset, this.length - 8);
        }
        const lo = first + this[++offset] * 2 ** 8 + this[++offset] * 2 ** 16 + this[++offset] * 2 ** 24;
        const hi = this[++offset] + this[++offset] * 2 ** 8 + this[++offset] * 2 ** 16 + last * 2 ** 24;
        return BigInt(lo) + (BigInt(hi) << BigInt(32));
      });
      Buffer4.prototype.readBigUInt64BE = defineBigIntMethod(function readBigUInt64BE(offset) {
        offset = offset >>> 0;
        validateNumber(offset, "offset");
        const first = this[offset];
        const last = this[offset + 7];
        if (first === void 0 || last === void 0) {
          boundsError(offset, this.length - 8);
        }
        const hi = first * 2 ** 24 + this[++offset] * 2 ** 16 + this[++offset] * 2 ** 8 + this[++offset];
        const lo = this[++offset] * 2 ** 24 + this[++offset] * 2 ** 16 + this[++offset] * 2 ** 8 + last;
        return (BigInt(hi) << BigInt(32)) + BigInt(lo);
      });
      Buffer4.prototype.readIntLE = function readIntLE(offset, byteLength2, noAssert) {
        offset = offset >>> 0;
        byteLength2 = byteLength2 >>> 0;
        if (!noAssert) checkOffset(offset, byteLength2, this.length);
        let val = this[offset];
        let mul = 1;
        let i = 0;
        while (++i < byteLength2 && (mul *= 256)) {
          val += this[offset + i] * mul;
        }
        mul *= 128;
        if (val >= mul) val -= Math.pow(2, 8 * byteLength2);
        return val;
      };
      Buffer4.prototype.readIntBE = function readIntBE(offset, byteLength2, noAssert) {
        offset = offset >>> 0;
        byteLength2 = byteLength2 >>> 0;
        if (!noAssert) checkOffset(offset, byteLength2, this.length);
        let i = byteLength2;
        let mul = 1;
        let val = this[offset + --i];
        while (i > 0 && (mul *= 256)) {
          val += this[offset + --i] * mul;
        }
        mul *= 128;
        if (val >= mul) val -= Math.pow(2, 8 * byteLength2);
        return val;
      };
      Buffer4.prototype.readInt8 = function readInt8(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 1, this.length);
        if (!(this[offset] & 128)) return this[offset];
        return (255 - this[offset] + 1) * -1;
      };
      Buffer4.prototype.readInt16LE = function readInt16LE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 2, this.length);
        const val = this[offset] | this[offset + 1] << 8;
        return val & 32768 ? val | 4294901760 : val;
      };
      Buffer4.prototype.readInt16BE = function readInt16BE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 2, this.length);
        const val = this[offset + 1] | this[offset] << 8;
        return val & 32768 ? val | 4294901760 : val;
      };
      Buffer4.prototype.readInt32LE = function readInt32LE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 4, this.length);
        return this[offset] | this[offset + 1] << 8 | this[offset + 2] << 16 | this[offset + 3] << 24;
      };
      Buffer4.prototype.readInt32BE = function readInt32BE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 4, this.length);
        return this[offset] << 24 | this[offset + 1] << 16 | this[offset + 2] << 8 | this[offset + 3];
      };
      Buffer4.prototype.readBigInt64LE = defineBigIntMethod(function readBigInt64LE(offset) {
        offset = offset >>> 0;
        validateNumber(offset, "offset");
        const first = this[offset];
        const last = this[offset + 7];
        if (first === void 0 || last === void 0) {
          boundsError(offset, this.length - 8);
        }
        const val = this[offset + 4] + this[offset + 5] * 2 ** 8 + this[offset + 6] * 2 ** 16 + (last << 24);
        return (BigInt(val) << BigInt(32)) + BigInt(first + this[++offset] * 2 ** 8 + this[++offset] * 2 ** 16 + this[++offset] * 2 ** 24);
      });
      Buffer4.prototype.readBigInt64BE = defineBigIntMethod(function readBigInt64BE(offset) {
        offset = offset >>> 0;
        validateNumber(offset, "offset");
        const first = this[offset];
        const last = this[offset + 7];
        if (first === void 0 || last === void 0) {
          boundsError(offset, this.length - 8);
        }
        const val = (first << 24) + // Overflow
        this[++offset] * 2 ** 16 + this[++offset] * 2 ** 8 + this[++offset];
        return (BigInt(val) << BigInt(32)) + BigInt(this[++offset] * 2 ** 24 + this[++offset] * 2 ** 16 + this[++offset] * 2 ** 8 + last);
      });
      Buffer4.prototype.readFloatLE = function readFloatLE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 4, this.length);
        return ieee754.read(this, offset, true, 23, 4);
      };
      Buffer4.prototype.readFloatBE = function readFloatBE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 4, this.length);
        return ieee754.read(this, offset, false, 23, 4);
      };
      Buffer4.prototype.readDoubleLE = function readDoubleLE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 8, this.length);
        return ieee754.read(this, offset, true, 52, 8);
      };
      Buffer4.prototype.readDoubleBE = function readDoubleBE(offset, noAssert) {
        offset = offset >>> 0;
        if (!noAssert) checkOffset(offset, 8, this.length);
        return ieee754.read(this, offset, false, 52, 8);
      };
      function checkInt(buf, value, offset, ext, max, min) {
        if (!Buffer4.isBuffer(buf)) throw new TypeError('"buffer" argument must be a Buffer instance');
        if (value > max || value < min) throw new RangeError('"value" argument is out of bounds');
        if (offset + ext > buf.length) throw new RangeError("Index out of range");
      }
      Buffer4.prototype.writeUintLE = Buffer4.prototype.writeUIntLE = function writeUIntLE(value, offset, byteLength2, noAssert) {
        value = +value;
        offset = offset >>> 0;
        byteLength2 = byteLength2 >>> 0;
        if (!noAssert) {
          const maxBytes = Math.pow(2, 8 * byteLength2) - 1;
          checkInt(this, value, offset, byteLength2, maxBytes, 0);
        }
        let mul = 1;
        let i = 0;
        this[offset] = value & 255;
        while (++i < byteLength2 && (mul *= 256)) {
          this[offset + i] = value / mul & 255;
        }
        return offset + byteLength2;
      };
      Buffer4.prototype.writeUintBE = Buffer4.prototype.writeUIntBE = function writeUIntBE(value, offset, byteLength2, noAssert) {
        value = +value;
        offset = offset >>> 0;
        byteLength2 = byteLength2 >>> 0;
        if (!noAssert) {
          const maxBytes = Math.pow(2, 8 * byteLength2) - 1;
          checkInt(this, value, offset, byteLength2, maxBytes, 0);
        }
        let i = byteLength2 - 1;
        let mul = 1;
        this[offset + i] = value & 255;
        while (--i >= 0 && (mul *= 256)) {
          this[offset + i] = value / mul & 255;
        }
        return offset + byteLength2;
      };
      Buffer4.prototype.writeUint8 = Buffer4.prototype.writeUInt8 = function writeUInt8(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 1, 255, 0);
        this[offset] = value & 255;
        return offset + 1;
      };
      Buffer4.prototype.writeUint16LE = Buffer4.prototype.writeUInt16LE = function writeUInt16LE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 2, 65535, 0);
        this[offset] = value & 255;
        this[offset + 1] = value >>> 8;
        return offset + 2;
      };
      Buffer4.prototype.writeUint16BE = Buffer4.prototype.writeUInt16BE = function writeUInt16BE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 2, 65535, 0);
        this[offset] = value >>> 8;
        this[offset + 1] = value & 255;
        return offset + 2;
      };
      Buffer4.prototype.writeUint32LE = Buffer4.prototype.writeUInt32LE = function writeUInt32LE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 4, 4294967295, 0);
        this[offset + 3] = value >>> 24;
        this[offset + 2] = value >>> 16;
        this[offset + 1] = value >>> 8;
        this[offset] = value & 255;
        return offset + 4;
      };
      Buffer4.prototype.writeUint32BE = Buffer4.prototype.writeUInt32BE = function writeUInt32BE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 4, 4294967295, 0);
        this[offset] = value >>> 24;
        this[offset + 1] = value >>> 16;
        this[offset + 2] = value >>> 8;
        this[offset + 3] = value & 255;
        return offset + 4;
      };
      function wrtBigUInt64LE(buf, value, offset, min, max) {
        checkIntBI(value, min, max, buf, offset, 7);
        let lo = Number(value & BigInt(4294967295));
        buf[offset++] = lo;
        lo = lo >> 8;
        buf[offset++] = lo;
        lo = lo >> 8;
        buf[offset++] = lo;
        lo = lo >> 8;
        buf[offset++] = lo;
        let hi = Number(value >> BigInt(32) & BigInt(4294967295));
        buf[offset++] = hi;
        hi = hi >> 8;
        buf[offset++] = hi;
        hi = hi >> 8;
        buf[offset++] = hi;
        hi = hi >> 8;
        buf[offset++] = hi;
        return offset;
      }
      function wrtBigUInt64BE(buf, value, offset, min, max) {
        checkIntBI(value, min, max, buf, offset, 7);
        let lo = Number(value & BigInt(4294967295));
        buf[offset + 7] = lo;
        lo = lo >> 8;
        buf[offset + 6] = lo;
        lo = lo >> 8;
        buf[offset + 5] = lo;
        lo = lo >> 8;
        buf[offset + 4] = lo;
        let hi = Number(value >> BigInt(32) & BigInt(4294967295));
        buf[offset + 3] = hi;
        hi = hi >> 8;
        buf[offset + 2] = hi;
        hi = hi >> 8;
        buf[offset + 1] = hi;
        hi = hi >> 8;
        buf[offset] = hi;
        return offset + 8;
      }
      Buffer4.prototype.writeBigUInt64LE = defineBigIntMethod(function writeBigUInt64LE(value, offset = 0) {
        return wrtBigUInt64LE(this, value, offset, BigInt(0), BigInt("0xffffffffffffffff"));
      });
      Buffer4.prototype.writeBigUInt64BE = defineBigIntMethod(function writeBigUInt64BE(value, offset = 0) {
        return wrtBigUInt64BE(this, value, offset, BigInt(0), BigInt("0xffffffffffffffff"));
      });
      Buffer4.prototype.writeIntLE = function writeIntLE(value, offset, byteLength2, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) {
          const limit = Math.pow(2, 8 * byteLength2 - 1);
          checkInt(this, value, offset, byteLength2, limit - 1, -limit);
        }
        let i = 0;
        let mul = 1;
        let sub = 0;
        this[offset] = value & 255;
        while (++i < byteLength2 && (mul *= 256)) {
          if (value < 0 && sub === 0 && this[offset + i - 1] !== 0) {
            sub = 1;
          }
          this[offset + i] = (value / mul >> 0) - sub & 255;
        }
        return offset + byteLength2;
      };
      Buffer4.prototype.writeIntBE = function writeIntBE(value, offset, byteLength2, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) {
          const limit = Math.pow(2, 8 * byteLength2 - 1);
          checkInt(this, value, offset, byteLength2, limit - 1, -limit);
        }
        let i = byteLength2 - 1;
        let mul = 1;
        let sub = 0;
        this[offset + i] = value & 255;
        while (--i >= 0 && (mul *= 256)) {
          if (value < 0 && sub === 0 && this[offset + i + 1] !== 0) {
            sub = 1;
          }
          this[offset + i] = (value / mul >> 0) - sub & 255;
        }
        return offset + byteLength2;
      };
      Buffer4.prototype.writeInt8 = function writeInt8(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 1, 127, -128);
        if (value < 0) value = 255 + value + 1;
        this[offset] = value & 255;
        return offset + 1;
      };
      Buffer4.prototype.writeInt16LE = function writeInt16LE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 2, 32767, -32768);
        this[offset] = value & 255;
        this[offset + 1] = value >>> 8;
        return offset + 2;
      };
      Buffer4.prototype.writeInt16BE = function writeInt16BE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 2, 32767, -32768);
        this[offset] = value >>> 8;
        this[offset + 1] = value & 255;
        return offset + 2;
      };
      Buffer4.prototype.writeInt32LE = function writeInt32LE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 4, 2147483647, -2147483648);
        this[offset] = value & 255;
        this[offset + 1] = value >>> 8;
        this[offset + 2] = value >>> 16;
        this[offset + 3] = value >>> 24;
        return offset + 4;
      };
      Buffer4.prototype.writeInt32BE = function writeInt32BE(value, offset, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) checkInt(this, value, offset, 4, 2147483647, -2147483648);
        if (value < 0) value = 4294967295 + value + 1;
        this[offset] = value >>> 24;
        this[offset + 1] = value >>> 16;
        this[offset + 2] = value >>> 8;
        this[offset + 3] = value & 255;
        return offset + 4;
      };
      Buffer4.prototype.writeBigInt64LE = defineBigIntMethod(function writeBigInt64LE(value, offset = 0) {
        return wrtBigUInt64LE(this, value, offset, -BigInt("0x8000000000000000"), BigInt("0x7fffffffffffffff"));
      });
      Buffer4.prototype.writeBigInt64BE = defineBigIntMethod(function writeBigInt64BE(value, offset = 0) {
        return wrtBigUInt64BE(this, value, offset, -BigInt("0x8000000000000000"), BigInt("0x7fffffffffffffff"));
      });
      function checkIEEE754(buf, value, offset, ext, max, min) {
        if (offset + ext > buf.length) throw new RangeError("Index out of range");
        if (offset < 0) throw new RangeError("Index out of range");
      }
      function writeFloat(buf, value, offset, littleEndian, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) {
          checkIEEE754(buf, value, offset, 4, 34028234663852886e22, -34028234663852886e22);
        }
        ieee754.write(buf, value, offset, littleEndian, 23, 4);
        return offset + 4;
      }
      Buffer4.prototype.writeFloatLE = function writeFloatLE(value, offset, noAssert) {
        return writeFloat(this, value, offset, true, noAssert);
      };
      Buffer4.prototype.writeFloatBE = function writeFloatBE(value, offset, noAssert) {
        return writeFloat(this, value, offset, false, noAssert);
      };
      function writeDouble(buf, value, offset, littleEndian, noAssert) {
        value = +value;
        offset = offset >>> 0;
        if (!noAssert) {
          checkIEEE754(buf, value, offset, 8, 17976931348623157e292, -17976931348623157e292);
        }
        ieee754.write(buf, value, offset, littleEndian, 52, 8);
        return offset + 8;
      }
      Buffer4.prototype.writeDoubleLE = function writeDoubleLE(value, offset, noAssert) {
        return writeDouble(this, value, offset, true, noAssert);
      };
      Buffer4.prototype.writeDoubleBE = function writeDoubleBE(value, offset, noAssert) {
        return writeDouble(this, value, offset, false, noAssert);
      };
      Buffer4.prototype.copy = function copy(target, targetStart, start, end) {
        if (!Buffer4.isBuffer(target)) throw new TypeError("argument should be a Buffer");
        if (!start) start = 0;
        if (!end && end !== 0) end = this.length;
        if (targetStart >= target.length) targetStart = target.length;
        if (!targetStart) targetStart = 0;
        if (end > 0 && end < start) end = start;
        if (end === start) return 0;
        if (target.length === 0 || this.length === 0) return 0;
        if (targetStart < 0) {
          throw new RangeError("targetStart out of bounds");
        }
        if (start < 0 || start >= this.length) throw new RangeError("Index out of range");
        if (end < 0) throw new RangeError("sourceEnd out of bounds");
        if (end > this.length) end = this.length;
        if (target.length - targetStart < end - start) {
          end = target.length - targetStart + start;
        }
        const len = end - start;
        if (this === target && typeof Uint8Array.prototype.copyWithin === "function") {
          this.copyWithin(targetStart, start, end);
        } else {
          Uint8Array.prototype.set.call(
            target,
            this.subarray(start, end),
            targetStart
          );
        }
        return len;
      };
      Buffer4.prototype.fill = function fill(val, start, end, encoding) {
        if (typeof val === "string") {
          if (typeof start === "string") {
            encoding = start;
            start = 0;
            end = this.length;
          } else if (typeof end === "string") {
            encoding = end;
            end = this.length;
          }
          if (encoding !== void 0 && typeof encoding !== "string") {
            throw new TypeError("encoding must be a string");
          }
          if (typeof encoding === "string" && !Buffer4.isEncoding(encoding)) {
            throw new TypeError("Unknown encoding: " + encoding);
          }
          if (val.length === 1) {
            const code = val.charCodeAt(0);
            if (encoding === "utf8" && code < 128 || encoding === "latin1") {
              val = code;
            }
          }
        } else if (typeof val === "number") {
          val = val & 255;
        } else if (typeof val === "boolean") {
          val = Number(val);
        }
        if (start < 0 || this.length < start || this.length < end) {
          throw new RangeError("Out of range index");
        }
        if (end <= start) {
          return this;
        }
        start = start >>> 0;
        end = end === void 0 ? this.length : end >>> 0;
        if (!val) val = 0;
        let i;
        if (typeof val === "number") {
          for (i = start; i < end; ++i) {
            this[i] = val;
          }
        } else {
          const bytes = Buffer4.isBuffer(val) ? val : Buffer4.from(val, encoding);
          const len = bytes.length;
          if (len === 0) {
            throw new TypeError('The value "' + val + '" is invalid for argument "value"');
          }
          for (i = 0; i < end - start; ++i) {
            this[i + start] = bytes[i % len];
          }
        }
        return this;
      };
      var errors = {};
      function E(sym, getMessage, Base) {
        errors[sym] = class NodeError extends Base {
          constructor() {
            super();
            Object.defineProperty(this, "message", {
              value: getMessage.apply(this, arguments),
              writable: true,
              configurable: true
            });
            this.name = `${this.name} [${sym}]`;
            this.stack;
            delete this.name;
          }
          get code() {
            return sym;
          }
          set code(value) {
            Object.defineProperty(this, "code", {
              configurable: true,
              enumerable: true,
              value,
              writable: true
            });
          }
          toString() {
            return `${this.name} [${sym}]: ${this.message}`;
          }
        };
      }
      E(
        "ERR_BUFFER_OUT_OF_BOUNDS",
        function(name) {
          if (name) {
            return `${name} is outside of buffer bounds`;
          }
          return "Attempt to access memory outside buffer bounds";
        },
        RangeError
      );
      E(
        "ERR_INVALID_ARG_TYPE",
        function(name, actual) {
          return `The "${name}" argument must be of type number. Received type ${typeof actual}`;
        },
        TypeError
      );
      E(
        "ERR_OUT_OF_RANGE",
        function(str, range, input) {
          let msg = `The value of "${str}" is out of range.`;
          let received = input;
          if (Number.isInteger(input) && Math.abs(input) > 2 ** 32) {
            received = addNumericalSeparator(String(input));
          } else if (typeof input === "bigint") {
            received = String(input);
            if (input > BigInt(2) ** BigInt(32) || input < -(BigInt(2) ** BigInt(32))) {
              received = addNumericalSeparator(received);
            }
            received += "n";
          }
          msg += ` It must be ${range}. Received ${received}`;
          return msg;
        },
        RangeError
      );
      function addNumericalSeparator(val) {
        let res = "";
        let i = val.length;
        const start = val[0] === "-" ? 1 : 0;
        for (; i >= start + 4; i -= 3) {
          res = `_${val.slice(i - 3, i)}${res}`;
        }
        return `${val.slice(0, i)}${res}`;
      }
      function checkBounds(buf, offset, byteLength2) {
        validateNumber(offset, "offset");
        if (buf[offset] === void 0 || buf[offset + byteLength2] === void 0) {
          boundsError(offset, buf.length - (byteLength2 + 1));
        }
      }
      function checkIntBI(value, min, max, buf, offset, byteLength2) {
        if (value > max || value < min) {
          const n = typeof min === "bigint" ? "n" : "";
          let range;
          if (byteLength2 > 3) {
            if (min === 0 || min === BigInt(0)) {
              range = `>= 0${n} and < 2${n} ** ${(byteLength2 + 1) * 8}${n}`;
            } else {
              range = `>= -(2${n} ** ${(byteLength2 + 1) * 8 - 1}${n}) and < 2 ** ${(byteLength2 + 1) * 8 - 1}${n}`;
            }
          } else {
            range = `>= ${min}${n} and <= ${max}${n}`;
          }
          throw new errors.ERR_OUT_OF_RANGE("value", range, value);
        }
        checkBounds(buf, offset, byteLength2);
      }
      function validateNumber(value, name) {
        if (typeof value !== "number") {
          throw new errors.ERR_INVALID_ARG_TYPE(name, "number", value);
        }
      }
      function boundsError(value, length, type) {
        if (Math.floor(value) !== value) {
          validateNumber(value, type);
          throw new errors.ERR_OUT_OF_RANGE(type || "offset", "an integer", value);
        }
        if (length < 0) {
          throw new errors.ERR_BUFFER_OUT_OF_BOUNDS();
        }
        throw new errors.ERR_OUT_OF_RANGE(
          type || "offset",
          `>= ${type ? 1 : 0} and <= ${length}`,
          value
        );
      }
      var INVALID_BASE64_RE = /[^+/0-9A-Za-z-_]/g;
      function base64clean(str) {
        str = str.split("=")[0];
        str = str.trim().replace(INVALID_BASE64_RE, "");
        if (str.length < 2) return "";
        while (str.length % 4 !== 0) {
          str = str + "=";
        }
        return str;
      }
      function utf8ToBytes(string, units) {
        units = units || Infinity;
        let codePoint;
        const length = string.length;
        let leadSurrogate = null;
        const bytes = [];
        for (let i = 0; i < length; ++i) {
          codePoint = string.charCodeAt(i);
          if (codePoint > 55295 && codePoint < 57344) {
            if (!leadSurrogate) {
              if (codePoint > 56319) {
                if ((units -= 3) > -1) bytes.push(239, 191, 189);
                continue;
              } else if (i + 1 === length) {
                if ((units -= 3) > -1) bytes.push(239, 191, 189);
                continue;
              }
              leadSurrogate = codePoint;
              continue;
            }
            if (codePoint < 56320) {
              if ((units -= 3) > -1) bytes.push(239, 191, 189);
              leadSurrogate = codePoint;
              continue;
            }
            codePoint = (leadSurrogate - 55296 << 10 | codePoint - 56320) + 65536;
          } else if (leadSurrogate) {
            if ((units -= 3) > -1) bytes.push(239, 191, 189);
          }
          leadSurrogate = null;
          if (codePoint < 128) {
            if ((units -= 1) < 0) break;
            bytes.push(codePoint);
          } else if (codePoint < 2048) {
            if ((units -= 2) < 0) break;
            bytes.push(
              codePoint >> 6 | 192,
              codePoint & 63 | 128
            );
          } else if (codePoint < 65536) {
            if ((units -= 3) < 0) break;
            bytes.push(
              codePoint >> 12 | 224,
              codePoint >> 6 & 63 | 128,
              codePoint & 63 | 128
            );
          } else if (codePoint < 1114112) {
            if ((units -= 4) < 0) break;
            bytes.push(
              codePoint >> 18 | 240,
              codePoint >> 12 & 63 | 128,
              codePoint >> 6 & 63 | 128,
              codePoint & 63 | 128
            );
          } else {
            throw new Error("Invalid code point");
          }
        }
        return bytes;
      }
      function asciiToBytes(str) {
        const byteArray = [];
        for (let i = 0; i < str.length; ++i) {
          byteArray.push(str.charCodeAt(i) & 255);
        }
        return byteArray;
      }
      function utf16leToBytes(str, units) {
        let c, hi, lo;
        const byteArray = [];
        for (let i = 0; i < str.length; ++i) {
          if ((units -= 2) < 0) break;
          c = str.charCodeAt(i);
          hi = c >> 8;
          lo = c % 256;
          byteArray.push(lo);
          byteArray.push(hi);
        }
        return byteArray;
      }
      function base64ToBytes(str) {
        return base64.toByteArray(base64clean(str));
      }
      function blitBuffer(src, dst, offset, length) {
        let i;
        for (i = 0; i < length; ++i) {
          if (i + offset >= dst.length || i >= src.length) break;
          dst[i + offset] = src[i];
        }
        return i;
      }
      function isInstance(obj, type) {
        return obj instanceof type || obj != null && obj.constructor != null && obj.constructor.name != null && obj.constructor.name === type.name;
      }
      function numberIsNaN(obj) {
        return obj !== obj;
      }
      var hexSliceLookupTable = (function() {
        const alphabet = "0123456789abcdef";
        const table = new Array(256);
        for (let i = 0; i < 16; ++i) {
          const i16 = i * 16;
          for (let j = 0; j < 16; ++j) {
            table[i16 + j] = alphabet[i] + alphabet[j];
          }
        }
        return table;
      })();
      function defineBigIntMethod(fn) {
        return typeof BigInt === "undefined" ? BufferBigIntNotDefined : fn;
      }
      function BufferBigIntNotDefined() {
        throw new Error("BigInt not supported");
      }
    }
  });

  // .agent/recovery/secure-exec/nodejs/src/bridge/index.ts
  var index_exports = {};
  __export(index_exports, {
    Buffer: () => Buffer3,
    CustomEvent: () => CustomEvent,
    Event: () => Event,
    EventTarget: () => EventTarget,
    Module: () => Module,
    ProcessExitError: () => ProcessExitError,
    SourceMap: () => SourceMap,
    TextDecoder: () => TextDecoder,
    TextEncoder: () => TextEncoder2,
    URL: () => URL2,
    URLSearchParams: () => URLSearchParams,
    _getActiveHandles: () => _getActiveHandles,
    _registerHandle: () => _registerHandle2,
    _unregisterHandle: () => _unregisterHandle2,
    _waitForActiveHandles: () => _waitForActiveHandles,
    childProcess: () => child_process_exports,
    clearImmediate: () => clearImmediate,
    clearInterval: () => clearInterval,
    clearTimeout: () => clearTimeout2,
    createRequire: () => createRequire,
    cryptoPolyfill: () => cryptoPolyfill,
    default: () => index_default,
    fs: () => fs_default,
    module: () => module_default,
    network: () => network_exports,
    os: () => os_default,
    process: () => process_default,
    setImmediate: () => setImmediate,
    setInterval: () => setInterval,
    setTimeout: () => setTimeout2,
    setupGlobals: () => setupGlobals
  });

  // .agent/recovery/secure-exec/nodejs/src/bridge/polyfills.ts
  function defineGlobal(name, value) {
    globalThis[name] = value;
  }
  if (typeof globalThis.global === "undefined") {
    defineGlobal("global", globalThis);
  }
  if (typeof globalThis.RegExp === "function" && !("__secureExecRgiEmojiCompat" in globalThis.RegExp)) {
    const NativeRegExp = globalThis.RegExp;
    const rgiEmojiPattern = "^\\p{RGI_Emoji}$";
    const rgiEmojiBaseClass = "[\\u{00A9}\\u{00AE}\\u{203C}\\u{2049}\\u{2122}\\u{2139}\\u{2194}-\\u{21AA}\\u{231A}-\\u{23FF}\\u{24C2}\\u{25AA}-\\u{27BF}\\u{2934}-\\u{2935}\\u{2B05}-\\u{2B55}\\u{3030}\\u{303D}\\u{3297}\\u{3299}\\u{1F000}-\\u{1FAFF}]";
    const rgiEmojiKeycap = "[#*0-9]\\uFE0F?\\u20E3";
    const rgiEmojiFallbackSource = "^(?:" + rgiEmojiKeycap + "|\\p{Regional_Indicator}{2}|" + rgiEmojiBaseClass + "(?:\\uFE0F|\\u200D(?:" + rgiEmojiKeycap + "|" + rgiEmojiBaseClass + ")|[\\u{1F3FB}-\\u{1F3FF}])*)$";
    try {
      new NativeRegExp(rgiEmojiPattern, "v");
    } catch (error) {
      if (String(error?.message ?? error).includes("RGI_Emoji")) {
        const CompatRegExp = function CompatRegExp2(pattern, flags) {
          const normalizedPattern = pattern instanceof NativeRegExp && flags === void 0 ? pattern.source : String(pattern);
          const normalizedFlags = flags === void 0 ? pattern instanceof NativeRegExp ? pattern.flags : "" : String(flags);
          try {
            return new NativeRegExp(pattern, flags);
          } catch (innerError) {
            if (normalizedPattern === rgiEmojiPattern && normalizedFlags === "v") {
              return new NativeRegExp(rgiEmojiFallbackSource, "u");
            }
            throw innerError;
          }
        };
        Object.setPrototypeOf(CompatRegExp, NativeRegExp);
        CompatRegExp.prototype = NativeRegExp.prototype;
        Object.defineProperty(CompatRegExp.prototype, "constructor", {
          value: CompatRegExp,
          writable: true,
          configurable: true
        });
        defineGlobal(
          "RegExp",
          Object.assign(CompatRegExp, { __secureExecRgiEmojiCompat: true })
        );
      }
    }
  }
  function withCode(error, code) {
    error.code = code;
    return error;
  }
  function createEncodingNotSupportedError(label) {
    return withCode(
      new RangeError(`The "${label}" encoding is not supported`),
      "ERR_ENCODING_NOT_SUPPORTED"
    );
  }
  function createEncodingInvalidDataError(encoding) {
    return withCode(
      new TypeError(`The encoded data was not valid for encoding ${encoding}`),
      "ERR_ENCODING_INVALID_ENCODED_DATA"
    );
  }
  function createInvalidDecodeInputError() {
    return withCode(
      new TypeError(
        'The "input" argument must be an instance of ArrayBuffer, SharedArrayBuffer, or ArrayBufferView.'
      ),
      "ERR_INVALID_ARG_TYPE"
    );
  }
  function trimAsciiWhitespace(value) {
    return value.replace(/^[\t\n\f\r ]+|[\t\n\f\r ]+$/g, "");
  }
  function normalizeEncodingLabel(label) {
    const normalized = trimAsciiWhitespace(
      label === void 0 ? "utf-8" : String(label)
    ).toLowerCase();
    switch (normalized) {
      case "utf-8":
      case "utf8":
      case "unicode-1-1-utf-8":
      case "unicode11utf8":
      case "unicode20utf8":
      case "x-unicode20utf8":
        return "utf-8";
      case "utf-16":
      case "utf-16le":
      case "ucs-2":
      case "ucs2":
      case "csunicode":
      case "iso-10646-ucs-2":
      case "unicode":
      case "unicodefeff":
        return "utf-16le";
      case "utf-16be":
      case "unicodefffe":
        return "utf-16be";
      default:
        throw createEncodingNotSupportedError(normalized);
    }
  }
  function toUint8Array(input) {
    if (input === void 0) {
      return new Uint8Array(0);
    }
    if (ArrayBuffer.isView(input)) {
      return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
    }
    if (input instanceof ArrayBuffer) {
      return new Uint8Array(input);
    }
    if (typeof SharedArrayBuffer !== "undefined" && input instanceof SharedArrayBuffer) {
      return new Uint8Array(input);
    }
    throw createInvalidDecodeInputError();
  }
  function encodeUtf8ScalarValue(codePoint, bytes) {
    if (codePoint <= 127) {
      bytes.push(codePoint);
      return;
    }
    if (codePoint <= 2047) {
      bytes.push(192 | codePoint >> 6, 128 | codePoint & 63);
      return;
    }
    if (codePoint <= 65535) {
      bytes.push(
        224 | codePoint >> 12,
        128 | codePoint >> 6 & 63,
        128 | codePoint & 63
      );
      return;
    }
    bytes.push(
      240 | codePoint >> 18,
      128 | codePoint >> 12 & 63,
      128 | codePoint >> 6 & 63,
      128 | codePoint & 63
    );
  }
  function encodeUtf8(input = "") {
    const value = String(input);
    const bytes = [];
    for (let index = 0; index < value.length; index += 1) {
      const codeUnit = value.charCodeAt(index);
      if (codeUnit >= 55296 && codeUnit <= 56319) {
        const nextIndex = index + 1;
        if (nextIndex < value.length) {
          const nextCodeUnit = value.charCodeAt(nextIndex);
          if (nextCodeUnit >= 56320 && nextCodeUnit <= 57343) {
            const codePoint = 65536 + (codeUnit - 55296 << 10) + (nextCodeUnit - 56320);
            encodeUtf8ScalarValue(codePoint, bytes);
            index = nextIndex;
            continue;
          }
        }
        encodeUtf8ScalarValue(65533, bytes);
        continue;
      }
      if (codeUnit >= 56320 && codeUnit <= 57343) {
        encodeUtf8ScalarValue(65533, bytes);
        continue;
      }
      encodeUtf8ScalarValue(codeUnit, bytes);
    }
    return new Uint8Array(bytes);
  }
  function appendCodePoint(output, codePoint) {
    if (codePoint <= 65535) {
      output.push(String.fromCharCode(codePoint));
      return;
    }
    const adjusted = codePoint - 65536;
    output.push(
      String.fromCharCode(55296 + (adjusted >> 10)),
      String.fromCharCode(56320 + (adjusted & 1023))
    );
  }
  function isContinuationByte(value) {
    return value >= 128 && value <= 191;
  }
  function decodeUtf8(bytes, fatal, stream, encoding) {
    const output = [];
    for (let index = 0; index < bytes.length; ) {
      const first = bytes[index];
      if (first <= 127) {
        output.push(String.fromCharCode(first));
        index += 1;
        continue;
      }
      let needed = 0;
      let codePoint = 0;
      if (first >= 194 && first <= 223) {
        needed = 1;
        codePoint = first & 31;
      } else if (first >= 224 && first <= 239) {
        needed = 2;
        codePoint = first & 15;
      } else if (first >= 240 && first <= 244) {
        needed = 3;
        codePoint = first & 7;
      } else {
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        index += 1;
        continue;
      }
      if (index + needed >= bytes.length) {
        if (stream) {
          return {
            text: output.join(""),
            pending: Array.from(bytes.slice(index))
          };
        }
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        break;
      }
      const second = bytes[index + 1];
      if (!isContinuationByte(second)) {
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        index += 1;
        continue;
      }
      if (first === 224 && second < 160 || first === 237 && second > 159 || first === 240 && second < 144 || first === 244 && second > 143) {
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        index += 1;
        continue;
      }
      codePoint = codePoint << 6 | second & 63;
      if (needed >= 2) {
        const third = bytes[index + 2];
        if (!isContinuationByte(third)) {
          if (fatal) {
            throw createEncodingInvalidDataError(encoding);
          }
          output.push("\uFFFD");
          index += 1;
          continue;
        }
        codePoint = codePoint << 6 | third & 63;
      }
      if (needed === 3) {
        const fourth = bytes[index + 3];
        if (!isContinuationByte(fourth)) {
          if (fatal) {
            throw createEncodingInvalidDataError(encoding);
          }
          output.push("\uFFFD");
          index += 1;
          continue;
        }
        codePoint = codePoint << 6 | fourth & 63;
      }
      if (codePoint >= 55296 && codePoint <= 57343) {
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        index += needed + 1;
        continue;
      }
      appendCodePoint(output, codePoint);
      index += needed + 1;
    }
    return { text: output.join(""), pending: [] };
  }
  function decodeUtf16(bytes, encoding, fatal, stream, bomSeen) {
    const output = [];
    let endian = encoding === "utf-16be" ? "be" : "le";
    if (!bomSeen && encoding === "utf-16le" && bytes.length >= 2) {
      if (bytes[0] === 254 && bytes[1] === 255) {
        endian = "be";
      }
    }
    for (let index = 0; index < bytes.length; ) {
      if (index + 1 >= bytes.length) {
        if (stream) {
          return {
            text: output.join(""),
            pending: Array.from(bytes.slice(index))
          };
        }
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        break;
      }
      const first = bytes[index];
      const second = bytes[index + 1];
      const codeUnit = endian === "le" ? first | second << 8 : first << 8 | second;
      index += 2;
      if (codeUnit >= 55296 && codeUnit <= 56319) {
        if (index + 1 >= bytes.length) {
          if (stream) {
            return {
              text: output.join(""),
              pending: Array.from(bytes.slice(index - 2))
            };
          }
          if (fatal) {
            throw createEncodingInvalidDataError(encoding);
          }
          output.push("\uFFFD");
          continue;
        }
        const nextFirst = bytes[index];
        const nextSecond = bytes[index + 1];
        const nextCodeUnit = endian === "le" ? nextFirst | nextSecond << 8 : nextFirst << 8 | nextSecond;
        if (nextCodeUnit >= 56320 && nextCodeUnit <= 57343) {
          const codePoint = 65536 + (codeUnit - 55296 << 10) + (nextCodeUnit - 56320);
          appendCodePoint(output, codePoint);
          index += 2;
          continue;
        }
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        continue;
      }
      if (codeUnit >= 56320 && codeUnit <= 57343) {
        if (fatal) {
          throw createEncodingInvalidDataError(encoding);
        }
        output.push("\uFFFD");
        continue;
      }
      output.push(String.fromCharCode(codeUnit));
    }
    return { text: output.join(""), pending: [] };
  }
  var PatchedTextEncoder = class {
    encode(input = "") {
      return encodeUtf8(input);
    }
    encodeInto(input, destination) {
      const value = String(input);
      let read = 0;
      let written = 0;
      for (let index = 0; index < value.length; index += 1) {
        const codeUnit = value.charCodeAt(index);
        let chunk = "";
        if (codeUnit >= 55296 && codeUnit <= 56319 && index + 1 < value.length) {
          const nextCodeUnit = value.charCodeAt(index + 1);
          if (nextCodeUnit >= 56320 && nextCodeUnit <= 57343) {
            chunk = value.slice(index, index + 2);
          }
        }
        if (chunk === "") {
          chunk = value[index] ?? "";
        }
        const encoded = encodeUtf8(chunk);
        if (written + encoded.length > destination.length) {
          break;
        }
        destination.set(encoded, written);
        written += encoded.length;
        read += chunk.length;
        if (chunk.length === 2) {
          index += 1;
        }
      }
      return { read, written };
    }
    get encoding() {
      return "utf-8";
    }
    get [Symbol.toStringTag]() {
      return "TextEncoder";
    }
  };
  var PatchedTextDecoder = class {
    normalizedEncoding;
    fatalFlag;
    ignoreBOMFlag;
    pendingBytes = [];
    bomSeen = false;
    constructor(label, options) {
      const normalizedOptions = options == null ? {} : Object(options);
      this.normalizedEncoding = normalizeEncodingLabel(label);
      this.fatalFlag = Boolean(
        normalizedOptions.fatal
      );
      this.ignoreBOMFlag = Boolean(
        normalizedOptions.ignoreBOM
      );
    }
    get encoding() {
      return this.normalizedEncoding;
    }
    get fatal() {
      return this.fatalFlag;
    }
    get ignoreBOM() {
      return this.ignoreBOMFlag;
    }
    get [Symbol.toStringTag]() {
      return "TextDecoder";
    }
    decode(input, options) {
      const normalizedOptions = options == null ? {} : Object(options);
      const stream = Boolean(
        normalizedOptions.stream
      );
      const incoming = toUint8Array(input);
      const merged = new Uint8Array(this.pendingBytes.length + incoming.length);
      merged.set(this.pendingBytes, 0);
      merged.set(incoming, this.pendingBytes.length);
      const decoded = this.normalizedEncoding === "utf-8" ? decodeUtf8(
        merged,
        this.fatalFlag,
        stream,
        this.normalizedEncoding
      ) : decodeUtf16(
        merged,
        this.normalizedEncoding,
        this.fatalFlag,
        stream,
        this.bomSeen
      );
      this.pendingBytes = decoded.pending;
      let text = decoded.text;
      if (!this.bomSeen && text.length > 0) {
        if (!this.ignoreBOMFlag && text.charCodeAt(0) === 65279) {
          text = text.slice(1);
        }
        this.bomSeen = true;
      }
      if (!stream && this.pendingBytes.length > 0) {
        const pending = this.pendingBytes;
        this.pendingBytes = [];
        if (this.fatalFlag) {
          throw createEncodingInvalidDataError(this.normalizedEncoding);
        }
        return text + "\uFFFD".repeat(Math.ceil(pending.length / 2));
      }
      return text;
    }
  };
  function normalizeAddEventListenerOptions(options) {
    if (typeof options === "boolean") {
      return {
        capture: options,
        once: false,
        passive: false
      };
    }
    if (options == null) {
      return {
        capture: false,
        once: false,
        passive: false
      };
    }
    const normalized = Object(options);
    return {
      capture: Boolean(normalized.capture),
      once: Boolean(normalized.once),
      passive: Boolean(normalized.passive),
      signal: normalized.signal
    };
  }
  function normalizeRemoveEventListenerOptions(options) {
    if (typeof options === "boolean") {
      return options;
    }
    if (options == null) {
      return false;
    }
    return Boolean(Object(options).capture);
  }
  function isAbortSignalLike(value) {
    return typeof value === "object" && value !== null && "aborted" in value && typeof value.addEventListener === "function" && typeof value.removeEventListener === "function";
  }
  var PatchedEvent = class {
    static NONE = 0;
    static CAPTURING_PHASE = 1;
    static AT_TARGET = 2;
    static BUBBLING_PHASE = 3;
    type;
    bubbles;
    cancelable;
    composed;
    detail = null;
    defaultPrevented = false;
    target = null;
    currentTarget = null;
    eventPhase = 0;
    returnValue = true;
    cancelBubble = false;
    timeStamp = Date.now();
    isTrusted = false;
    srcElement = null;
    inPassiveListener = false;
    propagationStopped = false;
    immediatePropagationStopped = false;
    constructor(type, init) {
      if (arguments.length === 0) {
        throw new TypeError("The event type must be provided");
      }
      const normalizedInit = init == null ? {} : Object(init);
      this.type = String(type);
      this.bubbles = Boolean(normalizedInit.bubbles);
      this.cancelable = Boolean(normalizedInit.cancelable);
      this.composed = Boolean(normalizedInit.composed);
    }
    get [Symbol.toStringTag]() {
      return "Event";
    }
    preventDefault() {
      if (this.cancelable && !this.inPassiveListener) {
        this.defaultPrevented = true;
        this.returnValue = false;
      }
    }
    stopPropagation() {
      this.propagationStopped = true;
      this.cancelBubble = true;
    }
    stopImmediatePropagation() {
      this.propagationStopped = true;
      this.immediatePropagationStopped = true;
      this.cancelBubble = true;
    }
    composedPath() {
      return this.target ? [this.target] : [];
    }
    _setPassive(value) {
      this.inPassiveListener = value;
    }
    _isPropagationStopped() {
      return this.propagationStopped;
    }
    _isImmediatePropagationStopped() {
      return this.immediatePropagationStopped;
    }
  };
  var PatchedCustomEvent = class extends PatchedEvent {
    constructor(type, init) {
      super(type, init);
      const normalizedInit = init == null ? null : Object(init);
      this.detail = normalizedInit && "detail" in normalizedInit ? normalizedInit.detail : null;
    }
    get [Symbol.toStringTag]() {
      return "CustomEvent";
    }
  };
  var PatchedEventTarget = class {
    listeners = /* @__PURE__ */ new Map();
    addEventListener(type, listener, options) {
      const normalized = normalizeAddEventListenerOptions(options);
      if (normalized.signal !== void 0 && !isAbortSignalLike(normalized.signal)) {
        throw new TypeError(
          'The "signal" option must be an instance of AbortSignal.'
        );
      }
      if (listener == null) {
        return void 0;
      }
      if (typeof listener !== "function" && (typeof listener !== "object" || listener === null)) {
        return void 0;
      }
      if (normalized.signal?.aborted) {
        return void 0;
      }
      const records = this.listeners.get(type) ?? [];
      const existing = records.find(
        (record2) => record2.listener === listener && record2.capture === normalized.capture
      );
      if (existing) {
        return void 0;
      }
      const record = {
        listener,
        capture: normalized.capture,
        once: normalized.once,
        passive: normalized.passive,
        kind: typeof listener === "function" ? "function" : "object",
        signal: normalized.signal
      };
      if (normalized.signal) {
        record.abortListener = () => {
          this.removeEventListener(type, listener, normalized.capture);
        };
        normalized.signal.addEventListener("abort", record.abortListener, {
          once: true
        });
      }
      records.push(record);
      this.listeners.set(type, records);
      return void 0;
    }
    removeEventListener(type, listener, options) {
      if (listener == null) {
        return;
      }
      const capture = normalizeRemoveEventListenerOptions(options);
      const records = this.listeners.get(type);
      if (!records) {
        return;
      }
      const nextRecords = records.filter((record) => {
        const match = record.listener === listener && record.capture === capture;
        if (match && record.signal && record.abortListener) {
          record.signal.removeEventListener("abort", record.abortListener);
        }
        return !match;
      });
      if (nextRecords.length === 0) {
        this.listeners.delete(type);
        return;
      }
      this.listeners.set(type, nextRecords);
    }
    dispatchEvent(event) {
      if (typeof event !== "object" || event === null || typeof event.type !== "string") {
        throw new TypeError("Argument 1 must be an Event");
      }
      const patchedEvent = event;
      const records = (this.listeners.get(patchedEvent.type) ?? []).slice();
      patchedEvent.target = this;
      patchedEvent.currentTarget = this;
      patchedEvent.eventPhase = 2;
      for (const record of records) {
        const active = this.listeners.get(patchedEvent.type)?.includes(record);
        if (!active) {
          continue;
        }
        if (record.once) {
          this.removeEventListener(patchedEvent.type, record.listener, record.capture);
        }
        patchedEvent._setPassive(record.passive);
        if (record.kind === "function") {
          record.listener.call(this, patchedEvent);
        } else {
          const handleEvent = record.listener.handleEvent;
          if (typeof handleEvent === "function") {
            handleEvent.call(record.listener, patchedEvent);
          }
        }
        patchedEvent._setPassive(false);
        if (patchedEvent._isImmediatePropagationStopped()) {
          break;
        }
        if (patchedEvent._isPropagationStopped()) {
          break;
        }
      }
      patchedEvent.currentTarget = null;
      patchedEvent.eventPhase = 0;
      return !patchedEvent.defaultPrevented;
    }
  };
  var TextEncoder2 = PatchedTextEncoder;
  var TextDecoder = PatchedTextDecoder;
  var Event = PatchedEvent;
  var CustomEvent = PatchedCustomEvent;
  var EventTarget = PatchedEventTarget;
  var AbortSignal = class extends EventTarget {
    constructor() {
      super();
      this.aborted = false;
      this.reason = void 0;
    }
    throwIfAborted() {
      if (this.aborted) {
        throw this.reason instanceof Error ? this.reason : new Error(String(this.reason ?? "AbortError"));
      }
    }
  };
  var AbortController = class {
    constructor() {
      this.signal = new AbortSignal();
    }
    abort(reason) {
      if (this.signal.aborted) {
        return;
      }
      this.signal.aborted = true;
      this.signal.reason = reason;
      this.signal.dispatchEvent(new Event("abort"));
    }
  };
  var WritableStream = class {
    constructor(sink = {}) {
      this._sink = sink;
    }
    getWriter() {
      const sink = this._sink;
      return {
        write(chunk) {
          return Promise.resolve(typeof sink.write === "function" ? sink.write(chunk) : void 0);
        },
        close() {
          return Promise.resolve(typeof sink.close === "function" ? sink.close() : void 0);
        },
        releaseLock() {
        }
      };
    }
  };
  var ReadableStream = class {
    constructor(source = {}) {
      this._queue = [];
      this._pending = [];
      this._closed = false;
      this._error = null;
      const flushPending = () => {
        while (this._pending.length > 0) {
          const waiter = this._pending.shift();
          if (this._error) {
            waiter.reject(this._error);
            continue;
          }
          if (this._queue.length > 0) {
            waiter.resolve({ value: this._queue.shift(), done: false });
            continue;
          }
          if (this._closed) {
            waiter.resolve({ value: void 0, done: true });
            continue;
          }
          this._pending.unshift(waiter);
          break;
        }
      };
      const controller = {
        enqueue: (value) => {
          if (this._closed || this._error) return;
          this._queue.push(value);
          flushPending();
        },
        close: () => {
          if (this._closed || this._error) return;
          this._closed = true;
          flushPending();
        },
        error: (error) => {
          if (this._closed || this._error) return;
          this._error = error instanceof Error ? error : new Error(String(error));
          flushPending();
        }
      };
      if (typeof source.start === "function") {
        Promise.resolve().then(() => source.start(controller)).catch((error) => controller.error(error));
      }
    }
    getReader() {
      return {
        read: () => {
          if (this._error) {
            return Promise.reject(this._error);
          }
          if (this._queue.length > 0) {
            return Promise.resolve({ value: this._queue.shift(), done: false });
          }
          if (this._closed) {
            return Promise.resolve({ value: void 0, done: true });
          }
          return new Promise((resolve, reject) => {
            this._pending.push({ resolve, reject });
          });
        },
        releaseLock() {
        }
      };
    }
  };
  defineGlobal("TextEncoder", TextEncoder2);
  defineGlobal("TextDecoder", TextDecoder);
  defineGlobal("Event", Event);
  defineGlobal("CustomEvent", CustomEvent);
  defineGlobal("EventTarget", EventTarget);
  defineGlobal("AbortSignal", AbortSignal);
  defineGlobal("AbortController", AbortController);
  defineGlobal("ReadableStream", typeof WebReadableStream === "function" ? WebReadableStream : ReadableStream);
  defineGlobal("WritableStream", typeof WebWritableStream === "function" ? WebWritableStream : WritableStream);
  if (typeof WebTransformStream === "function") {
    defineGlobal("TransformStream", WebTransformStream);
  }

  // .agent/recovery/secure-exec/shared/global-exposure.ts
  var NODE_CUSTOM_GLOBAL_INVENTORY = [
    {
      name: "_processConfig",
      classification: "hardened",
      rationale: "Bridge bootstrap configuration must not be replaced by sandbox code."
    },
    {
      name: "_osConfig",
      classification: "hardened",
      rationale: "Bridge bootstrap configuration must not be replaced by sandbox code."
    },
    {
      name: "bridge",
      classification: "hardened",
      rationale: "Bridge export object is runtime-owned control-plane state."
    },
    {
      name: "_registerHandle",
      classification: "hardened",
      rationale: "Active-handle lifecycle hook controls runtime completion semantics."
    },
    {
      name: "_unregisterHandle",
      classification: "hardened",
      rationale: "Active-handle lifecycle hook controls runtime completion semantics."
    },
    {
      name: "_waitForActiveHandles",
      classification: "hardened",
      rationale: "Active-handle lifecycle hook controls runtime completion semantics."
    },
    {
      name: "_getActiveHandles",
      classification: "hardened",
      rationale: "Bridge debug hook should not be replaced by sandbox code."
    },
    {
      name: "_childProcessDispatch",
      classification: "hardened",
      rationale: "Host-to-sandbox child-process callback dispatch entrypoint."
    },
    {
      name: "_childProcessModule",
      classification: "hardened",
      rationale: "Bridge-owned child_process module handle for require resolution."
    },
    {
      name: "_osModule",
      classification: "hardened",
      rationale: "Bridge-owned os module handle for require resolution."
    },
    {
      name: "_moduleModule",
      classification: "hardened",
      rationale: "Bridge-owned module module handle for require resolution."
    },
    {
      name: "_httpModule",
      classification: "hardened",
      rationale: "Bridge-owned http module handle for require resolution."
    },
    {
      name: "_httpsModule",
      classification: "hardened",
      rationale: "Bridge-owned https module handle for require resolution."
    },
    {
      name: "_http2Module",
      classification: "hardened",
      rationale: "Bridge-owned http2 module handle for require resolution."
    },
    {
      name: "_dnsModule",
      classification: "hardened",
      rationale: "Bridge-owned dns module handle for require resolution."
    },
    {
      name: "_dgramModule",
      classification: "hardened",
      rationale: "Bridge-owned dgram module handle for require resolution."
    },
    {
      name: "_netModule",
      classification: "hardened",
      rationale: "Bridge-owned net module handle for require resolution."
    },
    {
      name: "_tlsModule",
      classification: "hardened",
      rationale: "Bridge-owned tls module handle for require resolution."
    },
    {
      name: "_netSocketDispatch",
      classification: "hardened",
      rationale: "Host-to-sandbox net socket event dispatch entrypoint."
    },
    {
      name: "_httpServerDispatch",
      classification: "hardened",
      rationale: "Host-to-sandbox HTTP server dispatch entrypoint."
    },
    {
      name: "_httpServerUpgradeDispatch",
      classification: "hardened",
      rationale: "Host-to-sandbox HTTP upgrade dispatch entrypoint."
    },
    {
      name: "_httpServerConnectDispatch",
      classification: "hardened",
      rationale: "Host-to-sandbox HTTP CONNECT dispatch entrypoint."
    },
    {
      name: "_http2Dispatch",
      classification: "hardened",
      rationale: "Host-to-sandbox HTTP/2 event dispatch entrypoint."
    },
    {
      name: "_timerDispatch",
      classification: "hardened",
      rationale: "Host-to-sandbox timer callback dispatch entrypoint."
    },
    {
      name: "_upgradeSocketData",
      classification: "hardened",
      rationale: "Host-to-sandbox HTTP upgrade socket data dispatch entrypoint."
    },
    {
      name: "_upgradeSocketEnd",
      classification: "hardened",
      rationale: "Host-to-sandbox HTTP upgrade socket close dispatch entrypoint."
    },
    {
      name: "ProcessExitError",
      classification: "hardened",
      rationale: "Runtime-owned process-exit control-path error class."
    },
    {
      name: "_log",
      classification: "hardened",
      rationale: "Host console capture reference consumed by sandbox console shim."
    },
    {
      name: "_error",
      classification: "hardened",
      rationale: "Host console capture reference consumed by sandbox console shim."
    },
    {
      name: "_loadPolyfill",
      classification: "hardened",
      rationale: "Host module-loading bridge reference."
    },
    {
      name: "_resolveModule",
      classification: "hardened",
      rationale: "Host module-resolution bridge reference."
    },
    {
      name: "_loadFile",
      classification: "hardened",
      rationale: "Host file-loading bridge reference."
    },
    {
      name: "_resolveModuleSync",
      classification: "hardened",
      rationale: "Host synchronous module-resolution bridge reference."
    },
    {
      name: "_loadFileSync",
      classification: "hardened",
      rationale: "Host synchronous file-loading bridge reference."
    },
    {
      name: "_scheduleTimer",
      classification: "hardened",
      rationale: "Host timer bridge reference used by process timers."
    },
    {
      name: "_cryptoRandomFill",
      classification: "hardened",
      rationale: "Host entropy bridge reference for crypto.getRandomValues."
    },
    {
      name: "_cryptoRandomUUID",
      classification: "hardened",
      rationale: "Host entropy bridge reference for crypto.randomUUID."
    },
    {
      name: "_cryptoHashDigest",
      classification: "hardened",
      rationale: "Host crypto digest bridge reference."
    },
    {
      name: "_cryptoHmacDigest",
      classification: "hardened",
      rationale: "Host crypto HMAC bridge reference."
    },
    {
      name: "_cryptoPbkdf2",
      classification: "hardened",
      rationale: "Host crypto PBKDF2 bridge reference."
    },
    {
      name: "_cryptoScrypt",
      classification: "hardened",
      rationale: "Host crypto scrypt bridge reference."
    },
    {
      name: "_cryptoCipheriv",
      classification: "hardened",
      rationale: "Host crypto cipher bridge reference."
    },
    {
      name: "_cryptoDecipheriv",
      classification: "hardened",
      rationale: "Host crypto decipher bridge reference."
    },
    {
      name: "_cryptoCipherivCreate",
      classification: "hardened",
      rationale: "Host streaming cipher bridge reference."
    },
    {
      name: "_cryptoCipherivUpdate",
      classification: "hardened",
      rationale: "Host streaming cipher update bridge reference."
    },
    {
      name: "_cryptoCipherivFinal",
      classification: "hardened",
      rationale: "Host streaming cipher finalization bridge reference."
    },
    {
      name: "_cryptoSign",
      classification: "hardened",
      rationale: "Host crypto sign bridge reference."
    },
    {
      name: "_cryptoVerify",
      classification: "hardened",
      rationale: "Host crypto verify bridge reference."
    },
    {
      name: "_cryptoAsymmetricOp",
      classification: "hardened",
      rationale: "Host asymmetric crypto operation bridge reference."
    },
    {
      name: "_cryptoCreateKeyObject",
      classification: "hardened",
      rationale: "Host asymmetric key import bridge reference."
    },
    {
      name: "_cryptoGenerateKeyPairSync",
      classification: "hardened",
      rationale: "Host crypto key-pair generation bridge reference."
    },
    {
      name: "_cryptoGenerateKeySync",
      classification: "hardened",
      rationale: "Host symmetric crypto key generation bridge reference."
    },
    {
      name: "_cryptoGeneratePrimeSync",
      classification: "hardened",
      rationale: "Host prime generation bridge reference."
    },
    {
      name: "_cryptoDiffieHellman",
      classification: "hardened",
      rationale: "Host stateless Diffie-Hellman bridge reference."
    },
    {
      name: "_cryptoDiffieHellmanGroup",
      classification: "hardened",
      rationale: "Host Diffie-Hellman group bridge reference."
    },
    {
      name: "_cryptoDiffieHellmanSessionCreate",
      classification: "hardened",
      rationale: "Host Diffie-Hellman/ECDH session creation bridge reference."
    },
    {
      name: "_cryptoDiffieHellmanSessionCall",
      classification: "hardened",
      rationale: "Host Diffie-Hellman/ECDH session method bridge reference."
    },
    {
      name: "_cryptoSubtle",
      classification: "hardened",
      rationale: "Host WebCrypto subtle bridge reference."
    },
    {
      name: "_fsReadFile",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsWriteFile",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsReadFileBinary",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsWriteFileBinary",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsReadDir",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsMkdir",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsRmdir",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsExists",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsStat",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsUnlink",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsRename",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsChmod",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsChown",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsLink",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsSymlink",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsReadlink",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsLstat",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsTruncate",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fsUtimes",
      classification: "hardened",
      rationale: "Host filesystem bridge reference."
    },
    {
      name: "_fs",
      classification: "hardened",
      rationale: "Bridge filesystem facade consumed by fs polyfill."
    },
    {
      name: "_childProcessSpawnStart",
      classification: "hardened",
      rationale: "Host child_process bridge reference."
    },
    {
      name: "_childProcessPoll",
      classification: "hardened",
      rationale: "Host child_process bridge reference."
    },
    {
      name: "_childProcessStdinWrite",
      classification: "hardened",
      rationale: "Host child_process bridge reference."
    },
    {
      name: "_childProcessStdinClose",
      classification: "hardened",
      rationale: "Host child_process bridge reference."
    },
    {
      name: "_childProcessKill",
      classification: "hardened",
      rationale: "Host child_process bridge reference."
    },
    {
      name: "_childProcessSpawnSync",
      classification: "hardened",
      rationale: "Host child_process bridge reference."
    },
    {
      name: "_networkDnsLookupRaw",
      classification: "hardened",
      rationale: "Host network bridge reference."
    },
    {
      name: "_networkHttpRequestRaw",
      classification: "hardened",
      rationale: "Host network bridge reference."
    },
    {
      name: "_networkHttpServerListenRaw",
      classification: "hardened",
      rationale: "Host network bridge reference."
    },
    {
      name: "_networkHttpServerCloseRaw",
      classification: "hardened",
      rationale: "Host network bridge reference."
    },
    {
      name: "_networkHttpServerRespondRaw",
      classification: "hardened",
      rationale: "Host network bridge reference for sandbox HTTP server responses."
    },
    {
      name: "_networkHttpServerWaitRaw",
      classification: "hardened",
      rationale: "Host network bridge reference for sandbox HTTP server lifetime tracking."
    },
    {
      name: "_networkHttp2ServerListenRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 server listen bridge reference."
    },
    {
      name: "_networkHttp2ServerCloseRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 server close bridge reference."
    },
    {
      name: "_networkHttp2ServerWaitRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 server lifetime bridge reference."
    },
    {
      name: "_networkHttp2SessionConnectRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session connect bridge reference."
    },
    {
      name: "_networkHttp2SessionRequestRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session request bridge reference."
    },
    {
      name: "_networkHttp2SessionSettingsRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session settings bridge reference."
    },
    {
      name: "_networkHttp2SessionSetLocalWindowSizeRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session local-window bridge reference."
    },
    {
      name: "_networkHttp2SessionGoawayRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session GOAWAY bridge reference."
    },
    {
      name: "_networkHttp2SessionCloseRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session close bridge reference."
    },
    {
      name: "_networkHttp2SessionDestroyRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session destroy bridge reference."
    },
    {
      name: "_networkHttp2SessionWaitRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session lifetime bridge reference."
    },
    {
      name: "_networkHttp2ServerPollRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 server event-poll bridge reference."
    },
    {
      name: "_networkHttp2SessionPollRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 session event-poll bridge reference."
    },
    {
      name: "_networkHttp2StreamRespondRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 stream respond bridge reference."
    },
    {
      name: "_networkHttp2StreamPushStreamRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 push stream bridge reference."
    },
    {
      name: "_networkHttp2StreamWriteRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 stream write bridge reference."
    },
    {
      name: "_networkHttp2StreamEndRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 stream end bridge reference."
    },
    {
      name: "_networkHttp2StreamCloseRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 stream close bridge reference."
    },
    {
      name: "_networkHttp2StreamPauseRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 stream pause bridge reference."
    },
    {
      name: "_networkHttp2StreamResumeRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 stream resume bridge reference."
    },
    {
      name: "_networkHttp2StreamRespondWithFileRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 stream respondWithFile bridge reference."
    },
    {
      name: "_networkHttp2ServerRespondRaw",
      classification: "hardened",
      rationale: "Host HTTP/2 server-response bridge reference."
    },
    {
      name: "_upgradeSocketWriteRaw",
      classification: "hardened",
      rationale: "Host HTTP upgrade socket write bridge reference."
    },
    {
      name: "_upgradeSocketEndRaw",
      classification: "hardened",
      rationale: "Host HTTP upgrade socket half-close bridge reference."
    },
    {
      name: "_upgradeSocketDestroyRaw",
      classification: "hardened",
      rationale: "Host HTTP upgrade socket destroy bridge reference."
    },
    {
      name: "_netSocketConnectRaw",
      classification: "hardened",
      rationale: "Host net socket connect bridge reference."
    },
    {
      name: "_netSocketWaitConnectRaw",
      classification: "hardened",
      rationale: "Host net socket connect-wait bridge reference."
    },
    {
      name: "_netSocketReadRaw",
      classification: "hardened",
      rationale: "Host net socket read bridge reference."
    },
    {
      name: "_netSocketSetNoDelayRaw",
      classification: "hardened",
      rationale: "Host net socket no-delay bridge reference."
    },
    {
      name: "_netSocketSetKeepAliveRaw",
      classification: "hardened",
      rationale: "Host net socket keepalive bridge reference."
    },
    {
      name: "_netSocketWriteRaw",
      classification: "hardened",
      rationale: "Host net socket write bridge reference."
    },
    {
      name: "_netSocketEndRaw",
      classification: "hardened",
      rationale: "Host net socket end bridge reference."
    },
    {
      name: "_netSocketDestroyRaw",
      classification: "hardened",
      rationale: "Host net socket destroy bridge reference."
    },
    {
      name: "_netSocketUpgradeTlsRaw",
      classification: "hardened",
      rationale: "Host net socket TLS-upgrade bridge reference."
    },
    {
      name: "_netSocketGetTlsClientHelloRaw",
      classification: "hardened",
      rationale: "Host loopback TLS client-hello bridge reference."
    },
    {
      name: "_netSocketTlsQueryRaw",
      classification: "hardened",
      rationale: "Host TLS socket query bridge reference."
    },
    {
      name: "_tlsGetCiphersRaw",
      classification: "hardened",
      rationale: "Host TLS cipher-list bridge reference."
    },
    {
      name: "_netServerListenRaw",
      classification: "hardened",
      rationale: "Host net server listen bridge reference."
    },
    {
      name: "_netServerAcceptRaw",
      classification: "hardened",
      rationale: "Host net server accept bridge reference."
    },
    {
      name: "_netServerCloseRaw",
      classification: "hardened",
      rationale: "Host net server close bridge reference."
    },
    {
      name: "_dgramSocketCreateRaw",
      classification: "hardened",
      rationale: "Host dgram socket create bridge reference."
    },
    {
      name: "_dgramSocketBindRaw",
      classification: "hardened",
      rationale: "Host dgram socket bind bridge reference."
    },
    {
      name: "_dgramSocketRecvRaw",
      classification: "hardened",
      rationale: "Host dgram socket receive bridge reference."
    },
    {
      name: "_dgramSocketSendRaw",
      classification: "hardened",
      rationale: "Host dgram socket send bridge reference."
    },
    {
      name: "_dgramSocketCloseRaw",
      classification: "hardened",
      rationale: "Host dgram socket close bridge reference."
    },
    {
      name: "_dgramSocketAddressRaw",
      classification: "hardened",
      rationale: "Host dgram socket address bridge reference."
    },
    {
      name: "_dgramSocketSetBufferSizeRaw",
      classification: "hardened",
      rationale: "Host dgram socket buffer-size setter bridge reference."
    },
    {
      name: "_dgramSocketGetBufferSizeRaw",
      classification: "hardened",
      rationale: "Host dgram socket buffer-size getter bridge reference."
    },
    {
      name: "_sqliteConstantsRaw",
      classification: "hardened",
      rationale: "Host sqlite constants bridge reference."
    },
    {
      name: "_sqliteDatabaseOpenRaw",
      classification: "hardened",
      rationale: "Host sqlite database-open bridge reference."
    },
    {
      name: "_sqliteDatabaseCloseRaw",
      classification: "hardened",
      rationale: "Host sqlite database-close bridge reference."
    },
    {
      name: "_sqliteDatabaseExecRaw",
      classification: "hardened",
      rationale: "Host sqlite exec bridge reference."
    },
    {
      name: "_sqliteDatabaseQueryRaw",
      classification: "hardened",
      rationale: "Host sqlite query bridge reference."
    },
    {
      name: "_sqliteDatabasePrepareRaw",
      classification: "hardened",
      rationale: "Host sqlite prepare bridge reference."
    },
    {
      name: "_sqliteDatabaseLocationRaw",
      classification: "hardened",
      rationale: "Host sqlite location bridge reference."
    },
    {
      name: "_sqliteDatabaseCheckpointRaw",
      classification: "hardened",
      rationale: "Host sqlite checkpoint bridge reference."
    },
    {
      name: "_sqliteStatementRunRaw",
      classification: "hardened",
      rationale: "Host sqlite statement-run bridge reference."
    },
    {
      name: "_sqliteStatementGetRaw",
      classification: "hardened",
      rationale: "Host sqlite statement-get bridge reference."
    },
    {
      name: "_sqliteStatementAllRaw",
      classification: "hardened",
      rationale: "Host sqlite statement-all bridge reference."
    },
    {
      name: "_sqliteStatementColumnsRaw",
      classification: "hardened",
      rationale: "Host sqlite statement-columns bridge reference."
    },
    {
      name: "_sqliteStatementSetReturnArraysRaw",
      classification: "hardened",
      rationale: "Host sqlite statement return-arrays bridge reference."
    },
    {
      name: "_sqliteStatementSetReadBigIntsRaw",
      classification: "hardened",
      rationale: "Host sqlite statement read-bigints bridge reference."
    },
    {
      name: "_sqliteStatementSetAllowBareNamedParametersRaw",
      classification: "hardened",
      rationale: "Host sqlite bare-named-parameter bridge reference."
    },
    {
      name: "_sqliteStatementSetAllowUnknownNamedParametersRaw",
      classification: "hardened",
      rationale: "Host sqlite unknown-named-parameter bridge reference."
    },
    {
      name: "_sqliteStatementFinalizeRaw",
      classification: "hardened",
      rationale: "Host sqlite statement-finalize bridge reference."
    },
    {
      name: "_batchResolveModules",
      classification: "hardened",
      rationale: "Host bridge for batched module resolution to reduce IPC round-trips."
    },
    {
      name: "_ptySetRawMode",
      classification: "hardened",
      rationale: "Host PTY bridge reference for stdin.setRawMode()."
    },
    {
      name: "require",
      classification: "hardened",
      rationale: "Runtime-owned global require shim entrypoint."
    },
    {
      name: "_requireFrom",
      classification: "hardened",
      rationale: "Runtime-owned internal require shim used by module polyfill."
    },
    {
      name: "_dynamicImport",
      classification: "hardened",
      rationale: "Runtime-owned host callback reference for dynamic import resolution."
    },
    {
      name: "__dynamicImport",
      classification: "hardened",
      rationale: "Runtime-owned dynamic-import shim entrypoint."
    },
    {
      name: "_moduleCache",
      classification: "hardened",
      rationale: "Per-execution CommonJS/require cache \u2014 hardened via read-only Proxy to prevent cache poisoning."
    },
    {
      name: "_pendingModules",
      classification: "mutable-runtime-state",
      rationale: "Per-execution circular-load tracking state."
    },
    {
      name: "_currentModule",
      classification: "mutable-runtime-state",
      rationale: "Per-execution module resolution context."
    },
    {
      name: "_stdinData",
      classification: "mutable-runtime-state",
      rationale: "Per-execution stdin payload state."
    },
    {
      name: "_stdinPosition",
      classification: "mutable-runtime-state",
      rationale: "Per-execution stdin stream cursor state."
    },
    {
      name: "_stdinEnded",
      classification: "mutable-runtime-state",
      rationale: "Per-execution stdin completion state."
    },
    {
      name: "_stdinFlowMode",
      classification: "mutable-runtime-state",
      rationale: "Per-execution stdin flow-control state."
    },
    {
      name: "module",
      classification: "mutable-runtime-state",
      rationale: "Per-execution CommonJS module wrapper state."
    },
    {
      name: "exports",
      classification: "mutable-runtime-state",
      rationale: "Per-execution CommonJS module wrapper state."
    },
    {
      name: "__filename",
      classification: "mutable-runtime-state",
      rationale: "Per-execution CommonJS file context state."
    },
    {
      name: "__dirname",
      classification: "mutable-runtime-state",
      rationale: "Per-execution CommonJS file context state."
    },
    {
      name: "fetch",
      classification: "hardened",
      rationale: "Network fetch API global \u2014 must not be replaceable by sandbox code."
    },
    {
      name: "Headers",
      classification: "hardened",
      rationale: "Network Headers API global \u2014 must not be replaceable by sandbox code."
    },
    {
      name: "Request",
      classification: "hardened",
      rationale: "Network Request API global \u2014 must not be replaceable by sandbox code."
    },
    {
      name: "Response",
      classification: "hardened",
      rationale: "Network Response API global \u2014 must not be replaceable by sandbox code."
    },
    {
      name: "DOMException",
      classification: "hardened",
      rationale: "DOMException global stub for undici/bootstrap compatibility."
    },
    {
      name: "__importMetaResolve",
      classification: "hardened",
      rationale: "Internal import.meta.resolve helper for transformed ESM modules."
    },
    {
      name: "Blob",
      classification: "hardened",
      rationale: "Blob API global stub \u2014 must not be replaceable by sandbox code."
    },
    {
      name: "File",
      classification: "hardened",
      rationale: "File API global stub \u2014 must not be replaceable by sandbox code."
    },
    {
      name: "FormData",
      classification: "hardened",
      rationale: "FormData API global stub \u2014 must not be replaceable by sandbox code."
    }
  ];
  var HARDENED_NODE_CUSTOM_GLOBALS = NODE_CUSTOM_GLOBAL_INVENTORY.filter((entry) => entry.classification === "hardened").map((entry) => entry.name);
  var MUTABLE_NODE_CUSTOM_GLOBALS = NODE_CUSTOM_GLOBAL_INVENTORY.filter((entry) => entry.classification === "mutable-runtime-state").map((entry) => entry.name);
  function exposeGlobalBinding(target, name, value, options = {}) {
    const mutable = options.mutable === true;
    const enumerable = options.enumerable !== false;
    Object.defineProperty(target, name, {
      value,
      writable: mutable,
      configurable: mutable,
      enumerable
    });
  }
  function exposeCustomGlobal(name, value) {
    exposeGlobalBinding(globalThis, name, value);
  }
  function exposeMutableRuntimeStateGlobal(name, value) {
    exposeGlobalBinding(globalThis, name, value, {
      mutable: true
    });
  }

  // .agent/recovery/secure-exec/nodejs/src/bridge/dispatch.ts
  function encodeDispatchArgs(args) {
    return JSON.stringify(
      args,
      (_key, value) => value === void 0 ? { __secureExecDispatchType: "undefined" } : value
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

  // .agent/recovery/secure-exec/nodejs/src/bridge/active-handles.ts
  var HANDLE_DISPATCH = {
    register: "kernelHandleRegister",
    unregister: "kernelHandleUnregister",
    list: "kernelHandleList"
  };
  var _activeHandles = /* @__PURE__ */ new Map();
  var _waitResolvers = [];
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
    let remaining = _activeHandles.size;
    try {
      bridgeDispatchSync(HANDLE_DISPATCH.unregister, id);
    } catch {
    }
    if (remaining === 0 && _waitResolvers.length > 0) {
      const resolvers = _waitResolvers;
      _waitResolvers = [];
      resolvers.forEach((r) => r());
    }
  }
  function _waitForActiveHandles() {
    const getPendingTimerCount = globalThis._getPendingTimerCount;
    const waitForTimerDrain = globalThis._waitForTimerDrain;
    const hasHandles = _getActiveHandles().length > 0;
    const hasTimers = typeof getPendingTimerCount === "function" && getPendingTimerCount() > 0;
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
    });
  }
  function _getActiveHandles() {
    return Array.from(_activeHandles.values());
  }
  exposeCustomGlobal("_registerHandle", _registerHandle2);
  exposeCustomGlobal("_unregisterHandle", _unregisterHandle2);
  exposeCustomGlobal("_waitForActiveHandles", _waitForActiveHandles);
  exposeCustomGlobal("_getActiveHandles", _getActiveHandles);

  // .agent/recovery/secure-exec/nodejs/src/bridge/fs.ts
  var import_buffer = __toESM(require_buffer(), 1);
  var O_RDONLY = 0;
  var O_WRONLY = 1;
  var O_RDWR = 2;
  var O_CREAT = 64;
  var O_EXCL = 128;
  var O_TRUNC = 512;
  var O_APPEND = 1024;
  var Stats = class {
    dev;
    ino;
    mode;
    nlink;
    uid;
    gid;
    rdev;
    size;
    blksize;
    blocks;
    atimeMs;
    mtimeMs;
    ctimeMs;
    birthtimeMs;
    atime;
    mtime;
    ctime;
    birthtime;
    constructor(init) {
      this.dev = init.dev ?? 0;
      this.ino = init.ino ?? 0;
      this.mode = init.mode;
      this.nlink = init.nlink ?? 1;
      this.uid = init.uid ?? 0;
      this.gid = init.gid ?? 0;
      this.rdev = init.rdev ?? 0;
      this.size = init.size;
      this.blksize = init.blksize ?? 4096;
      this.blocks = init.blocks ?? Math.ceil(init.size / 512);
      this.atimeMs = init.atimeMs ?? Date.now();
      this.mtimeMs = init.mtimeMs ?? Date.now();
      this.ctimeMs = init.ctimeMs ?? Date.now();
      this.birthtimeMs = init.birthtimeMs ?? Date.now();
      this.atime = new Date(this.atimeMs);
      this.mtime = new Date(this.mtimeMs);
      this.ctime = new Date(this.ctimeMs);
      this.birthtime = new Date(this.birthtimeMs);
    }
    isFile() {
      return (this.mode & 61440) === 32768;
    }
    isDirectory() {
      return (this.mode & 61440) === 16384;
    }
    isSymbolicLink() {
      return (this.mode & 61440) === 40960;
    }
    isBlockDevice() {
      return false;
    }
    isCharacterDevice() {
      return false;
    }
    isFIFO() {
      return false;
    }
    isSocket() {
      return false;
    }
  };
  var Dirent = class {
    name;
    parentPath;
    path;
    // Deprecated alias for parentPath
    _isDir;
    constructor(name, isDir, parentPath = "") {
      this.name = name;
      this._isDir = isDir;
      this.parentPath = parentPath;
      this.path = parentPath;
    }
    isFile() {
      return !this._isDir;
    }
    isDirectory() {
      return this._isDir;
    }
    isSymbolicLink() {
      return false;
    }
    isBlockDevice() {
      return false;
    }
    isCharacterDevice() {
      return false;
    }
    isFIFO() {
      return false;
    }
    isSocket() {
      return false;
    }
  };
  var Dir = class {
    path;
    _entries = null;
    _index = 0;
    _closed = false;
    constructor(dirPath) {
      this.path = dirPath;
    }
    _load() {
      if (this._entries === null) {
        this._entries = fs.readdirSync(this.path, { withFileTypes: true });
      }
      return this._entries;
    }
    readSync() {
      if (this._closed) throw new Error("Directory handle was closed");
      const entries = this._load();
      if (this._index >= entries.length) return null;
      return entries[this._index++];
    }
    async read() {
      return this.readSync();
    }
    closeSync() {
      this._closed = true;
    }
    async close() {
      this.closeSync();
    }
    async *[Symbol.asyncIterator]() {
      const entries = this._load();
      for (const entry of entries) {
        if (this._closed) return;
        yield entry;
      }
      this._closed = true;
    }
  };
  var FILE_HANDLE_READ_CHUNK_BYTES = 64 * 1024;
  var FILE_HANDLE_READ_BUFFER_BYTES = 16 * 1024;
  var FILE_HANDLE_MAX_READ_BYTES = 2 ** 31 - 1;
  function createAbortError(reason) {
    const error = new Error("The operation was aborted");
    error.name = "AbortError";
    error.code = "ABORT_ERR";
    if (reason !== void 0) {
      error.cause = reason;
    }
    return error;
  }
  function validateAbortSignal(signal) {
    if (signal === void 0) {
      return void 0;
    }
    if (signal === null || typeof signal !== "object" || typeof signal.aborted !== "boolean" || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function") {
      const error = new TypeError(
        'The "signal" argument must be an instance of AbortSignal'
      );
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    return signal;
  }
  function throwIfAborted(signal) {
    if (signal?.aborted) {
      throw createAbortError(signal.reason);
    }
  }
  function waitForNextTick() {
    return new Promise((resolve) => process.nextTick(resolve));
  }
  function createInternalAssertionError(message) {
    const error = new Error(message);
    error.code = "ERR_INTERNAL_ASSERTION";
    return error;
  }
  function createOutOfRangeError(name, range, received) {
    const error = new RangeError(
      `The value of "${name}" is out of range. It must be ${range}. Received ${String(received)}`
    );
    error.code = "ERR_OUT_OF_RANGE";
    return error;
  }
  function formatInvalidArgReceived(actual) {
    if (actual === null) {
      return "Received null";
    }
    if (actual === void 0) {
      return "Received undefined";
    }
    if (typeof actual === "string") {
      return `Received type string ('${actual}')`;
    }
    if (typeof actual === "number") {
      return `Received type number (${String(actual)})`;
    }
    if (typeof actual === "boolean") {
      return `Received type boolean (${String(actual)})`;
    }
    if (typeof actual === "bigint") {
      return `Received type bigint (${actual.toString()}n)`;
    }
    if (typeof actual === "symbol") {
      return `Received type symbol (${String(actual)})`;
    }
    if (typeof actual === "function") {
      return actual.name ? `Received function ${actual.name}` : "Received function";
    }
    if (Array.isArray(actual)) {
      return "Received an instance of Array";
    }
    if (actual && typeof actual === "object") {
      const constructorName = actual.constructor?.name;
      if (constructorName) {
        return `Received an instance of ${constructorName}`;
      }
    }
    return `Received type ${typeof actual} (${String(actual)})`;
  }
  function createInvalidArgTypeError(name, expected, actual) {
    const error = new TypeError(
      `The "${name}" argument must be ${expected}. ${formatInvalidArgReceived(actual)}`
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }
  function createInvalidArgValueError(name, message) {
    const error = new TypeError(
      `The argument '${name}' ${message}`
    );
    error.code = "ERR_INVALID_ARG_VALUE";
    return error;
  }
  function createInvalidEncodingError(encoding) {
    const printable = typeof encoding === "string" ? `'${encoding}'` : encoding === void 0 ? "undefined" : encoding === null ? "null" : String(encoding);
    const error = new TypeError(
      `The argument 'encoding' is invalid encoding. Received ${printable}`
    );
    error.code = "ERR_INVALID_ARG_VALUE";
    return error;
  }
  function toUint8ArrayChunk(chunk, encoding) {
    if (typeof chunk === "string") {
      return import_buffer.Buffer.from(chunk, encoding ?? "utf8");
    }
    if (import_buffer.Buffer.isBuffer(chunk)) {
      return new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
    }
    if (chunk instanceof Uint8Array) {
      return chunk;
    }
    if (ArrayBuffer.isView(chunk)) {
      return new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
    }
    throw createInvalidArgTypeError("data", "a string, Buffer, TypedArray, or DataView", chunk);
  }
  async function* iterateWriteChunks(data, encoding) {
    if (typeof data === "string" || ArrayBuffer.isView(data)) {
      yield toUint8ArrayChunk(data, encoding);
      return;
    }
    if (data && typeof data[Symbol.asyncIterator] === "function") {
      for await (const chunk of data) {
        yield toUint8ArrayChunk(chunk, encoding);
      }
      return;
    }
    if (data && typeof data[Symbol.iterator] === "function") {
      for (const chunk of data) {
        yield toUint8ArrayChunk(chunk, encoding);
      }
      return;
    }
    throw createInvalidArgTypeError("data", "a string, Buffer, TypedArray, DataView, or Iterable", data);
  }
  var FileHandle = class _FileHandle {
    _fd;
    _closing = false;
    _closed = false;
    _listeners = /* @__PURE__ */ new Map();
    constructor(fd) {
      this._fd = fd;
    }
    static _assertHandle(handle) {
      if (!(handle instanceof _FileHandle)) {
        throw createInternalAssertionError("handle must be an instance of FileHandle");
      }
      return handle;
    }
    _emitCloseOnce() {
      if (this._closed) {
        this._fd = -1;
        this.emit("close");
        return;
      }
      this._closed = true;
      this._fd = -1;
      this.emit("close");
    }
    _resolvePath() {
      if (this._fd < 0) {
        return null;
      }
      return _fdGetPath.applySync(void 0, [this._fd]);
    }
    get fd() {
      return this._fd;
    }
    get closed() {
      return this._closed;
    }
    on(event, listener) {
      const listeners = this._listeners.get(event) ?? [];
      listeners.push(listener);
      this._listeners.set(event, listeners);
      return this;
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener(...args);
      };
      wrapper._originalListener = listener;
      return this.on(event, wrapper);
    }
    off(event, listener) {
      const listeners = this._listeners.get(event);
      if (!listeners) {
        return this;
      }
      const index = listeners.findIndex(
        (candidate) => candidate === listener || candidate._originalListener === listener
      );
      if (index !== -1) {
        listeners.splice(index, 1);
      }
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    emit(event, ...args) {
      const listeners = this._listeners.get(event);
      if (!listeners || listeners.length === 0) {
        return false;
      }
      for (const listener of listeners.slice()) {
        listener(...args);
      }
      return true;
    }
    async close() {
      const handle = _FileHandle._assertHandle(this);
      if (handle._closing || handle._closed) {
        if (handle._fd < 0) {
          throw createFsError("EBADF", "EBADF: bad file descriptor, close", "close");
        }
      }
      handle._closing = true;
      try {
        fs.closeSync(handle._fd);
        handle._emitCloseOnce();
      } finally {
        handle._closing = false;
      }
    }
    async stat() {
      const handle = _FileHandle._assertHandle(this);
      return fs.fstatSync(handle.fd);
    }
    async sync() {
      const handle = _FileHandle._assertHandle(this);
      fs.fsyncSync(handle.fd);
    }
    async datasync() {
      return this.sync();
    }
    async truncate(len) {
      const handle = _FileHandle._assertHandle(this);
      fs.ftruncateSync(handle.fd, len);
    }
    async chmod(mode) {
      const handle = _FileHandle._assertHandle(this);
      const path = handle._resolvePath();
      if (!path) {
        throw createFsError("EBADF", "EBADF: bad file descriptor", "chmod");
      }
      fs.chmodSync(path, mode);
    }
    async chown(uid, gid) {
      const handle = _FileHandle._assertHandle(this);
      const path = handle._resolvePath();
      if (!path) {
        throw createFsError("EBADF", "EBADF: bad file descriptor", "chown");
      }
      fs.chownSync(path, uid, gid);
    }
    async utimes(atime, mtime) {
      const handle = _FileHandle._assertHandle(this);
      const path = handle._resolvePath();
      if (!path) {
        throw createFsError("EBADF", "EBADF: bad file descriptor", "utimes");
      }
      fs.utimesSync(path, atime, mtime);
    }
    async read(buffer, offset, length, position) {
      const handle = _FileHandle._assertHandle(this);
      let target = buffer;
      let readOffset = offset;
      let readLength = length;
      let readPosition = position;
      if (target !== null && typeof target === "object" && !ArrayBuffer.isView(target)) {
        readOffset = target.offset;
        readLength = target.length;
        readPosition = target.position;
        target = target.buffer ?? null;
      }
      if (target === null) {
        target = import_buffer.Buffer.alloc(FILE_HANDLE_READ_BUFFER_BYTES);
      }
      if (!ArrayBuffer.isView(target)) {
        throw createInvalidArgTypeError("buffer", "an instance of ArrayBufferView", target);
      }
      const normalizedOffset = readOffset ?? 0;
      const normalizedLength = readLength ?? target.byteLength - normalizedOffset;
      const bytesRead = fs.readSync(
        handle.fd,
        target,
        normalizedOffset,
        normalizedLength,
        readPosition ?? null
      );
      return { bytesRead, buffer: target };
    }
    async write(buffer, offsetOrPosition, lengthOrEncoding, position) {
      const handle = _FileHandle._assertHandle(this);
      if (typeof buffer === "string") {
        const encoding = typeof lengthOrEncoding === "string" ? lengthOrEncoding : "utf8";
        if (encoding === "hex" && buffer.length % 2 !== 0) {
          throw createInvalidArgValueError("encoding", `is invalid for data of length ${buffer.length}`);
        }
        const bytesWritten2 = fs.writeSync(handle.fd, import_buffer.Buffer.from(buffer, encoding), 0, void 0, offsetOrPosition ?? null);
        return { bytesWritten: bytesWritten2, buffer };
      }
      if (!ArrayBuffer.isView(buffer)) {
        throw createInvalidArgTypeError("buffer", "a string, Buffer, TypedArray, or DataView", buffer);
      }
      const offset = offsetOrPosition ?? 0;
      const length = typeof lengthOrEncoding === "number" ? lengthOrEncoding : void 0;
      const bytesWritten = fs.writeSync(handle.fd, buffer, offset, length, position ?? null);
      return { bytesWritten, buffer };
    }
    async readFile(options) {
      const handle = _FileHandle._assertHandle(this);
      const normalized = typeof options === "string" ? { encoding: options } : options ?? void 0;
      const signal = validateAbortSignal(normalized?.signal);
      const encoding = normalized?.encoding ?? void 0;
      const stats = await handle.stat();
      if (stats.size > FILE_HANDLE_MAX_READ_BYTES) {
        const error = new RangeError("File size is greater than 2 GiB");
        error.code = "ERR_FS_FILE_TOO_LARGE";
        throw error;
      }
      await waitForNextTick();
      throwIfAborted(signal);
      const chunks = [];
      let totalLength = 0;
      while (true) {
        throwIfAborted(signal);
        const chunk = import_buffer.Buffer.alloc(FILE_HANDLE_READ_CHUNK_BYTES);
        const { bytesRead } = await handle.read(chunk, 0, chunk.byteLength, null);
        if (bytesRead === 0) {
          break;
        }
        chunks.push(chunk.subarray(0, bytesRead));
        totalLength += bytesRead;
        if (totalLength > FILE_HANDLE_MAX_READ_BYTES) {
          const error = new RangeError("File size is greater than 2 GiB");
          error.code = "ERR_FS_FILE_TOO_LARGE";
          throw error;
        }
        await waitForNextTick();
      }
      const result = import_buffer.Buffer.concat(chunks, totalLength);
      return encoding ? result.toString(encoding) : result;
    }
    async writeFile(data, options) {
      const handle = _FileHandle._assertHandle(this);
      const normalized = typeof options === "string" ? { encoding: options } : options ?? void 0;
      const signal = validateAbortSignal(normalized?.signal);
      const encoding = normalized?.encoding ?? void 0;
      await waitForNextTick();
      throwIfAborted(signal);
      for await (const chunk of iterateWriteChunks(data, encoding)) {
        throwIfAborted(signal);
        await handle.write(chunk, 0, chunk.byteLength, void 0);
        await waitForNextTick();
      }
    }
    async appendFile(data, options) {
      return this.writeFile(data, options);
    }
    createReadStream(options) {
      _FileHandle._assertHandle(this);
      return new ReadStream(null, { ...options ?? {}, fd: this });
    }
    createWriteStream(options) {
      _FileHandle._assertHandle(this);
      return new WriteStream(null, { ...options ?? {}, fd: this });
    }
  };
  function isArrayBufferView(value) {
    return ArrayBuffer.isView(value);
  }
  function createInvalidPropertyTypeError(propertyPath, actual) {
    let received;
    if (actual === null) {
      received = "Received null";
    } else if (typeof actual === "string") {
      received = `Received type string ('${actual}')`;
    } else {
      received = `Received type ${typeof actual} (${String(actual)})`;
    }
    const error = new TypeError(
      `The "${propertyPath}" property must be of type function. ${received}`
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }
  function validateCallback(callback, name = "cb") {
    if (typeof callback !== "function") {
      throw createInvalidArgTypeError(name, "of type function", callback);
    }
  }
  function validateEncodingValue(encoding) {
    if (encoding === void 0 || encoding === null) {
      return;
    }
    if (typeof encoding !== "string" || !import_buffer.Buffer.isEncoding(encoding)) {
      throw createInvalidEncodingError(encoding);
    }
  }
  function validateEncodingOption(options) {
    if (typeof options === "string") {
      validateEncodingValue(options);
      return;
    }
    if (options && typeof options === "object" && "encoding" in options) {
      validateEncodingValue(options.encoding);
    }
  }
  function normalizePathLike(path, name = "path") {
    if (typeof path === "string") {
      return path;
    }
    if (import_buffer.Buffer.isBuffer(path)) {
      return path.toString("utf8");
    }
    if (path instanceof URL) {
      if (path.protocol === "file:") {
        return path.pathname;
      }
      throw createInvalidArgTypeError(name, "of type string or an instance of Buffer or URL", path);
    }
    throw createInvalidArgTypeError(name, "of type string or an instance of Buffer or URL", path);
  }
  function tryNormalizeExistsPath(path) {
    try {
      return normalizePathLike(path);
    } catch {
      return null;
    }
  }
  function normalizeNumberArgument(name, value, options = {}) {
    const { min = 0, max = 2147483647, allowNegativeOne = false } = options;
    if (typeof value !== "number") {
      throw createInvalidArgTypeError(name, "of type number", value);
    }
    if (!Number.isFinite(value) || !Number.isInteger(value)) {
      throw createOutOfRangeError(name, "an integer", value);
    }
    if (allowNegativeOne && value === -1 || value >= min && value <= max) {
      return value;
    }
    throw createOutOfRangeError(name, `>= ${min} && <= ${max}`, value);
  }
  function normalizeModeArgument(mode, name = "mode") {
    if (typeof mode === "string") {
      if (!/^[0-7]+$/.test(mode)) {
        throw createInvalidArgValueError(name, "must be a 32-bit unsigned integer or an octal string. Received '" + mode + "'");
      }
      return parseInt(mode, 8);
    }
    return normalizeNumberArgument(name, mode, { min: 0, max: 4294967295 });
  }
  function normalizeOpenModeArgument(mode) {
    if (mode === void 0 || mode === null) {
      return void 0;
    }
    return normalizeModeArgument(mode);
  }
  function validateWriteStreamStartOption(options) {
    if (options?.start === void 0) {
      return;
    }
    if (typeof options.start !== "number") {
      throw createInvalidArgTypeError("start", "of type number", options.start);
    }
    if (!Number.isFinite(options.start) || !Number.isInteger(options.start) || options.start < 0) {
      throw createOutOfRangeError("start", ">= 0", options.start);
    }
  }
  function validateBooleanOption(name, value) {
    if (value === void 0) {
      return void 0;
    }
    if (typeof value !== "boolean") {
      throw createInvalidArgTypeError(name, "of type boolean", value);
    }
    return value;
  }
  function validateAbortSignalOption(name, signal) {
    if (signal === void 0) {
      return void 0;
    }
    if (signal === null || typeof signal !== "object" || typeof signal.aborted !== "boolean" || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function") {
      const error = new TypeError(
        `The "${name}" property must be an instance of AbortSignal. ${formatInvalidArgReceived(signal)}`
      );
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    return signal;
  }
  function createUnsupportedWatcherError(api) {
    return new Error(`fs.${api} is not supported in sandbox \u2014 use polling`);
  }
  function normalizeWatchOptions(options, allowString) {
    let normalized;
    if (options === void 0 || options === null) {
      normalized = {};
    } else if (typeof options === "string") {
      if (!allowString) {
        throw createInvalidArgTypeError("options", "of type object", options);
      }
      validateEncodingValue(options);
      normalized = { encoding: options };
    } else if (typeof options === "object") {
      normalized = options;
    } else {
      throw createInvalidArgTypeError(
        "options",
        allowString ? "one of type string or object" : "of type object",
        options
      );
    }
    validateBooleanOption("options.persistent", normalized.persistent);
    validateBooleanOption("options.recursive", normalized.recursive);
    validateEncodingOption(normalized);
    const signal = validateAbortSignalOption("options.signal", normalized.signal);
    return {
      persistent: normalized.persistent,
      recursive: normalized.recursive,
      encoding: normalized.encoding,
      signal
    };
  }
  function normalizeWatchArguments(path, optionsOrListener, listener) {
    normalizePathLike(path);
    let options = optionsOrListener;
    let resolvedListener = listener;
    if (typeof optionsOrListener === "function") {
      options = void 0;
      resolvedListener = optionsOrListener;
    }
    if (resolvedListener !== void 0 && typeof resolvedListener !== "function") {
      throw createInvalidArgTypeError("listener", "of type function", resolvedListener);
    }
    return normalizeWatchOptions(options, true);
  }
  function normalizeWatchFileArguments(path, optionsOrListener, listener) {
    normalizePathLike(path);
    let options = {};
    let resolvedListener = listener;
    if (typeof optionsOrListener === "function") {
      resolvedListener = optionsOrListener;
    } else if (optionsOrListener === void 0 || optionsOrListener === null) {
      options = {};
    } else if (typeof optionsOrListener === "object") {
      options = optionsOrListener;
    } else {
      throw createInvalidArgTypeError("listener", "of type function", optionsOrListener);
    }
    if (typeof resolvedListener !== "function") {
      throw createInvalidArgTypeError("listener", "of type function", resolvedListener);
    }
    if (options.interval !== void 0 && typeof options.interval !== "number") {
      throw createInvalidArgTypeError("interval", "of type number", options.interval);
    }
  }
  async function* createUnsupportedPromisesWatchIterator(path, options) {
    const normalized = normalizeWatchOptions(options, false);
    normalizePathLike(path);
    throwIfAborted(normalized.signal);
    throw createUnsupportedWatcherError("promises.watch");
  }
  function isReadWriteOptionsObject(value) {
    return value === null || value === void 0 || typeof value === "object" && !Array.isArray(value);
  }
  function normalizeOptionalPosition(value) {
    if (value === void 0 || value === null || value === -1) {
      return null;
    }
    if (typeof value === "bigint") {
      return Number(value);
    }
    if (typeof value !== "number" || !Number.isInteger(value)) {
      throw createInvalidArgTypeError("position", "an integer", value);
    }
    return value;
  }
  function normalizeOffsetLength(bufferByteLength, offsetValue, lengthValue) {
    const offset = offsetValue ?? 0;
    if (typeof offset !== "number" || !Number.isInteger(offset)) {
      throw createInvalidArgTypeError("offset", "an integer", offset);
    }
    if (offset < 0 || offset > bufferByteLength) {
      throw createOutOfRangeError("offset", `>= 0 && <= ${bufferByteLength}`, offset);
    }
    const defaultLength = bufferByteLength - offset;
    const length = lengthValue ?? defaultLength;
    if (typeof length !== "number" || !Number.isInteger(length)) {
      throw createInvalidArgTypeError("length", "an integer", length);
    }
    if (length < 0 || length > 2147483647) {
      throw createOutOfRangeError("length", ">= 0 && <= 2147483647", length);
    }
    if (offset + length > bufferByteLength) {
      throw createOutOfRangeError("length", `>= 0 && <= ${bufferByteLength - offset}`, length);
    }
    return { offset, length };
  }
  function normalizeReadSyncArgs(buffer, offsetOrOptions, length, position) {
    if (!isArrayBufferView(buffer)) {
      throw createInvalidArgTypeError("buffer", "an instance of Buffer, TypedArray, or DataView", buffer);
    }
    if (length === void 0 && position === void 0 && isReadWriteOptionsObject(offsetOrOptions)) {
      const options = offsetOrOptions ?? {};
      const { offset: offset2, length: length2 } = normalizeOffsetLength(
        buffer.byteLength,
        options.offset,
        options.length
      );
      return {
        buffer,
        offset: offset2,
        length: length2,
        position: normalizeOptionalPosition(options.position)
      };
    }
    const { offset, length: normalizedLength } = normalizeOffsetLength(
      buffer.byteLength,
      offsetOrOptions,
      length
    );
    return {
      buffer,
      offset,
      length: normalizedLength,
      position: normalizeOptionalPosition(position)
    };
  }
  function normalizeWriteSyncArgs(buffer, offsetOrPosition, lengthOrEncoding, position) {
    if (typeof buffer === "string") {
      if (lengthOrEncoding === void 0 && position === void 0 && isReadWriteOptionsObject(offsetOrPosition)) {
        const options = offsetOrPosition ?? {};
        const encoding = typeof options.encoding === "string" ? options.encoding : void 0;
        return {
          buffer,
          offset: 0,
          length: import_buffer.Buffer.byteLength(buffer, encoding),
          position: normalizeOptionalPosition(options.position),
          encoding
        };
      }
      if (offsetOrPosition !== void 0 && offsetOrPosition !== null && typeof offsetOrPosition !== "number") {
        throw createInvalidArgTypeError("position", "an integer", offsetOrPosition);
      }
      return {
        buffer,
        offset: 0,
        length: import_buffer.Buffer.byteLength(buffer, typeof lengthOrEncoding === "string" ? lengthOrEncoding : void 0),
        position: normalizeOptionalPosition(offsetOrPosition),
        encoding: typeof lengthOrEncoding === "string" ? lengthOrEncoding : void 0
      };
    }
    if (!isArrayBufferView(buffer)) {
      throw createInvalidArgTypeError("buffer", "a string, Buffer, TypedArray, or DataView", buffer);
    }
    if (lengthOrEncoding === void 0 && position === void 0 && isReadWriteOptionsObject(offsetOrPosition)) {
      const options = offsetOrPosition ?? {};
      const { offset: offset2, length: length2 } = normalizeOffsetLength(
        buffer.byteLength,
        options.offset,
        options.length
      );
      return {
        buffer,
        offset: offset2,
        length: length2,
        position: normalizeOptionalPosition(options.position)
      };
    }
    const { offset, length } = normalizeOffsetLength(
      buffer.byteLength,
      offsetOrPosition,
      typeof lengthOrEncoding === "number" ? lengthOrEncoding : void 0
    );
    return {
      buffer,
      offset,
      length,
      position: normalizeOptionalPosition(position)
    };
  }
  function normalizeFdInteger(fd) {
    return normalizeNumberArgument("fd", fd);
  }
  function normalizeIoVectorBuffers(buffers) {
    if (!Array.isArray(buffers)) {
      throw createInvalidArgTypeError("buffers", "an ArrayBufferView[]", buffers);
    }
    for (const buffer of buffers) {
      if (!isArrayBufferView(buffer)) {
        throw createInvalidArgTypeError("buffers", "an ArrayBufferView[]", buffers);
      }
    }
    return buffers;
  }
  function validateStreamFsOverride(streamFs, required) {
    if (streamFs === void 0) {
      return void 0;
    }
    if (streamFs === null || typeof streamFs !== "object") {
      throw createInvalidArgTypeError("options.fs", "an object", streamFs);
    }
    const typed = streamFs;
    for (const key of required) {
      if (typeof typed[key] !== "function") {
        throw createInvalidPropertyTypeError(`options.fs.${String(key)}`, typed[key]);
      }
    }
    return typed;
  }
  function normalizeStreamFd(fd) {
    if (fd === void 0) {
      return void 0;
    }
    if (fd instanceof FileHandle) {
      return fd;
    }
    return normalizeNumberArgument("fd", fd);
  }
  function normalizeStreamPath(pathValue, fd) {
    if (pathValue === null) {
      if (fd === void 0) {
        throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", pathValue);
      }
      return null;
    }
    if (typeof pathValue === "string" || import_buffer.Buffer.isBuffer(pathValue)) {
      return pathValue;
    }
    if (pathValue instanceof URL) {
      if (pathValue.protocol === "file:") {
        return pathValue.pathname;
      }
      throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", pathValue);
    }
    throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", pathValue);
  }
  function normalizeStreamStartEnd(options) {
    const start = options?.start;
    const end = options?.end;
    if (start !== void 0 && typeof start !== "number") {
      throw createInvalidArgTypeError("start", "of type number", start);
    }
    if (end !== void 0 && typeof end !== "number") {
      throw createInvalidArgTypeError("end", "of type number", end);
    }
    const normalizedStart = start;
    const normalizedEnd = end;
    if (normalizedStart !== void 0 && (!Number.isFinite(normalizedStart) || normalizedStart < 0)) {
      throw createOutOfRangeError("start", ">= 0", start);
    }
    if (normalizedEnd !== void 0 && (!Number.isFinite(normalizedEnd) || normalizedEnd < 0)) {
      throw createOutOfRangeError("end", ">= 0", end);
    }
    if (normalizedStart !== void 0 && normalizedEnd !== void 0 && normalizedStart > normalizedEnd) {
      throw createOutOfRangeError("start", `<= "end" (here: ${normalizedEnd})`, normalizedStart);
    }
    const highWaterMarkCandidate = options?.highWaterMark ?? options?.bufferSize;
    const highWaterMark = typeof highWaterMarkCandidate === "number" && Number.isFinite(highWaterMarkCandidate) && highWaterMarkCandidate > 0 ? Math.floor(highWaterMarkCandidate) : 65536;
    return {
      start: normalizedStart,
      end: normalizedEnd,
      highWaterMark,
      autoClose: options?.autoClose !== false
    };
  }
  var ReadStream = class {
    constructor(filePath, _options) {
      this._options = _options;
      const fdOption = normalizeStreamFd(_options?.fd);
      const optionsRecord = _options ?? {};
      const streamState = normalizeStreamStartEnd(optionsRecord);
      this.path = filePath;
      this.start = streamState.start;
      this.end = streamState.end;
      this.autoClose = streamState.autoClose;
      this.readableHighWaterMark = streamState.highWaterMark;
      this.readableEncoding = _options?.encoding ?? null;
      this._position = this.start ?? null;
      this._remaining = this.end !== void 0 ? this.end - (this.start ?? 0) + 1 : null;
      this._signal = validateAbortSignal(_options?.signal);
      if (fdOption instanceof FileHandle) {
        if (_options?.fs !== void 0) {
          const error = new Error("The FileHandle with fs method is not implemented");
          error.code = "ERR_METHOD_NOT_IMPLEMENTED";
          throw error;
        }
        this._fileHandle = fdOption;
        this.fd = fdOption.fd;
        this.pending = false;
        this._handleCloseListener = () => {
          if (!this.closed) {
            this.closed = true;
            this.destroyed = true;
            this.readable = false;
            this.emit("close");
          }
        };
        this._fileHandle.on("close", this._handleCloseListener);
      } else {
        this._streamFs = validateStreamFsOverride(_options?.fs, ["open", "read", "close"]);
        if (typeof fdOption === "number") {
          this.fd = fdOption;
          this.pending = false;
        }
      }
      if (this._signal) {
        if (this._signal.aborted) {
          queueMicrotask(() => {
            void this._abort(this._signal?.reason);
          });
        } else {
          this._signal.addEventListener("abort", () => {
            void this._abort(this._signal?.reason);
          });
        }
      }
      if (this.fd === null) {
        queueMicrotask(() => {
          void this._openIfNeeded();
        });
      }
    }
    _options;
    bytesRead = 0;
    path;
    pending = true;
    readable = true;
    readableAborted = false;
    readableDidRead = false;
    readableEncoding = null;
    readableEnded = false;
    readableFlowing = null;
    readableHighWaterMark = 65536;
    readableLength = 0;
    readableObjectMode = false;
    destroyed = false;
    closed = false;
    errored = null;
    fd = null;
    autoClose = true;
    start;
    end;
    _listeners = /* @__PURE__ */ new Map();
    _started = false;
    _reading = false;
    _readScheduled = false;
    _opening = false;
    _remaining = null;
    _position = null;
    _fileHandle = null;
    _streamFs;
    _signal;
    _handleCloseListener;
    _emitOpen(fd) {
      this.fd = fd;
      this.pending = false;
      this.emit("open", fd);
      if (this._started || this.readableFlowing) {
        this._scheduleRead();
      }
    }
    async _openIfNeeded() {
      if (this.fd !== null || this._opening || this.destroyed || this.closed) {
        return;
      }
      const pathStr = typeof this.path === "string" ? this.path : this.path instanceof import_buffer.Buffer ? this.path.toString() : null;
      if (!pathStr) {
        this._handleStreamError(createFsError("EBADF", "EBADF: bad file descriptor", "read"));
        return;
      }
      this._opening = true;
      const opener = (this._streamFs?.open ?? fs.open).bind(this._streamFs ?? fs);
      opener(pathStr, "r", 438, (error, fd) => {
        this._opening = false;
        if (error || typeof fd !== "number") {
          this._handleStreamError(error ?? createFsError("EBADF", "EBADF: bad file descriptor", "open"));
          return;
        }
        this._emitOpen(fd);
      });
    }
    async _closeUnderlying() {
      if (this._fileHandle) {
        if (!this._fileHandle.closed) {
          await this._fileHandle.close();
        }
        return;
      }
      if (this.fd !== null && this.fd >= 0) {
        const fd = this.fd;
        const closer = (this._streamFs?.close ?? fs.close).bind(this._streamFs ?? fs);
        await new Promise((resolve) => {
          closer(fd, () => resolve());
        });
        this.fd = -1;
      }
    }
    _scheduleRead() {
      if (this._readScheduled || this._reading || this.readableFlowing === false || this.destroyed || this.closed) {
        return;
      }
      this._readScheduled = true;
      queueMicrotask(() => {
        this._readScheduled = false;
        void this._readNextChunk();
      });
    }
    async _readNextChunk() {
      if (this._reading || this.destroyed || this.closed || this.readableFlowing === false) {
        return;
      }
      throwIfAborted(this._signal);
      if (this.fd === null) {
        await this._openIfNeeded();
        return;
      }
      if (this._remaining === 0) {
        await this._finishReadable();
        return;
      }
      const nextLength = this._remaining === null ? this.readableHighWaterMark : Math.min(this.readableHighWaterMark, this._remaining);
      const target = import_buffer.Buffer.alloc(nextLength);
      this._reading = true;
      const onRead = async (error, bytesRead = 0) => {
        this._reading = false;
        if (error) {
          this._handleStreamError(error);
          return;
        }
        if (bytesRead === 0) {
          await this._finishReadable();
          return;
        }
        this.bytesRead += bytesRead;
        this.readableDidRead = true;
        if (typeof this._position === "number") {
          this._position += bytesRead;
        }
        if (this._remaining !== null) {
          this._remaining -= bytesRead;
        }
        const chunk = target.subarray(0, bytesRead);
        this.emit("data", this.readableEncoding ? chunk.toString(this.readableEncoding) : import_buffer.Buffer.from(chunk));
        if (this._remaining === 0) {
          await this._finishReadable();
          return;
        }
        this._scheduleRead();
      };
      if (this._fileHandle) {
        try {
          const result = await this._fileHandle.read(target, 0, nextLength, this._position);
          await onRead(null, result.bytesRead);
        } catch (error) {
          await onRead(error);
        }
        return;
      }
      const reader = (this._streamFs?.read ?? fs.read).bind(this._streamFs ?? fs);
      reader(this.fd, target, 0, nextLength, this._position, (error, bytesRead) => {
        void onRead(error, bytesRead ?? 0);
      });
    }
    async _finishReadable() {
      if (this.readableEnded) {
        return;
      }
      this.readable = false;
      this.readableEnded = true;
      this.emit("end");
      if (this.autoClose) {
        this.destroy();
      }
    }
    _handleStreamError(error) {
      if (this.closed) {
        return;
      }
      this.errored = error;
      this.emit("error", error);
      if (this.autoClose) {
        this.destroy();
      } else {
        this.readable = false;
      }
    }
    async _abort(reason) {
      if (this.closed || this.destroyed) {
        return;
      }
      this.readableAborted = true;
      this.errored = createAbortError(reason);
      this.emit("error", this.errored);
      if (this._fileHandle) {
        this.destroyed = true;
        this.readable = false;
        this.closed = true;
        this.emit("close");
        return;
      }
      if (this.autoClose) {
        this.destroy();
        return;
      }
      this.closed = true;
      this.emit("close");
    }
    async _readAllContent() {
      const chunks = [];
      let totalLength = 0;
      const savedFlowing = this.readableFlowing;
      this.readableFlowing = false;
      while (this._remaining !== 0) {
        if (this.fd === null) {
          await this._openIfNeeded();
        }
        if (this.fd === null) {
          break;
        }
        const nextLength = this._remaining === null ? FILE_HANDLE_READ_CHUNK_BYTES : Math.min(FILE_HANDLE_READ_CHUNK_BYTES, this._remaining);
        const target = import_buffer.Buffer.alloc(nextLength);
        let bytesRead = 0;
        if (this._fileHandle) {
          bytesRead = (await this._fileHandle.read(target, 0, nextLength, this._position)).bytesRead;
        } else {
          bytesRead = fs.readSync(this.fd, target, 0, nextLength, this._position);
        }
        if (bytesRead === 0) {
          break;
        }
        const chunk = target.subarray(0, bytesRead);
        chunks.push(chunk);
        totalLength += bytesRead;
        if (typeof this._position === "number") {
          this._position += bytesRead;
        }
        if (this._remaining !== null) {
          this._remaining -= bytesRead;
        }
      }
      this.readableFlowing = savedFlowing;
      return import_buffer.Buffer.concat(chunks, totalLength);
    }
    on(event, listener) {
      const listeners = this._listeners.get(event) ?? [];
      listeners.push(listener);
      this._listeners.set(event, listeners);
      if (event === "data") {
        this._started = true;
        this.readableFlowing = true;
        this._scheduleRead();
      }
      return this;
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener(...args);
      };
      wrapper._originalListener = listener;
      return this.on(event, wrapper);
    }
    off(event, listener) {
      const listeners = this._listeners.get(event);
      if (!listeners) {
        return this;
      }
      const index = listeners.findIndex(
        (fn) => fn === listener || fn._originalListener === listener
      );
      if (index >= 0) {
        listeners.splice(index, 1);
      }
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    removeAllListeners(event) {
      if (event === void 0) {
        this._listeners.clear();
      } else {
        this._listeners.delete(event);
      }
      return this;
    }
    emit(event, ...args) {
      const listeners = this._listeners.get(event);
      if (!listeners?.length) {
        return false;
      }
      listeners.slice().forEach((listener) => listener(...args));
      return true;
    }
    read() {
      return null;
    }
    pipe(destination, _options) {
      this.on("data", (chunk) => {
        destination.write(chunk);
      });
      this.on("end", () => {
        destination.end?.();
      });
      this.resume();
      return destination;
    }
    unpipe(_destination) {
      return this;
    }
    pause() {
      this.readableFlowing = false;
      return this;
    }
    resume() {
      this._started = true;
      this.readableFlowing = true;
      this._scheduleRead();
      return this;
    }
    setEncoding(encoding) {
      this.readableEncoding = encoding;
      return this;
    }
    destroy(error) {
      if (this.destroyed) {
        return this;
      }
      this.destroyed = true;
      this.readable = false;
      if (error) {
        this.errored = error;
        this.emit("error", error);
      }
      queueMicrotask(() => {
        void this._closeUnderlying().then(() => {
          if (!this.closed) {
            this.closed = true;
            this.emit("close");
          }
        });
      });
      return this;
    }
    close(callback) {
      this.destroy();
      if (callback) {
        queueMicrotask(() => callback(null));
      }
    }
    async *[Symbol.asyncIterator]() {
      const content = await this._readAllContent();
      yield this.readableEncoding ? content.toString(this.readableEncoding) : content;
    }
  };
  var MAX_WRITE_STREAM_BYTES = 16 * 1024 * 1024;
  var WriteStream = class {
    constructor(filePath, _options) {
      this._options = _options;
      const fdOption = normalizeStreamFd(_options?.fd);
      this.path = filePath;
      this.autoClose = _options?.autoClose !== false;
      this._streamFs = validateStreamFsOverride(_options?.fs, ["open", "close", "write"]);
      if (_options?.fs !== void 0) {
        validateStreamFsOverride(_options?.fs, ["writev"]);
      }
      if (fdOption instanceof FileHandle) {
        this._fileHandle = fdOption;
        this.fd = fdOption.fd;
        return;
      }
      if (typeof fdOption === "number") {
        this.fd = fdOption;
        return;
      }
      const pathStr = typeof this.path === "string" ? this.path : this.path instanceof import_buffer.Buffer ? this.path.toString() : null;
      if (!pathStr) {
        throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
      }
      this.fd = fs.openSync(pathStr, _options?.flags ?? "w", _options?.mode);
      queueMicrotask(() => {
        if (this.fd !== null && this.fd >= 0) {
          this.emit("open", this.fd);
        }
      });
    }
    _options;
    bytesWritten = 0;
    path;
    pending = false;
    writable = true;
    writableAborted = false;
    writableEnded = false;
    writableFinished = false;
    writableHighWaterMark = 16384;
    writableLength = 0;
    writableObjectMode = false;
    writableCorked = 0;
    destroyed = false;
    closed = false;
    errored = null;
    writableNeedDrain = false;
    fd = null;
    autoClose = true;
    _chunks = [];
    _listeners = /* @__PURE__ */ new Map();
    _fileHandle = null;
    _streamFs;
    async _closeUnderlying() {
      if (this._fileHandle) {
        if (!this._fileHandle.closed) {
          await this._fileHandle.close();
        }
        return;
      }
      if (this.fd !== null && this.fd >= 0) {
        const fd = this.fd;
        const closer = (this._streamFs?.close ?? fs.close).bind(this._streamFs ?? fs);
        await new Promise((resolve) => {
          closer(fd, () => resolve());
        });
        this.fd = -1;
      }
    }
    close(callback) {
      queueMicrotask(() => {
        void this._closeUnderlying().then(() => {
          if (!this.closed) {
            this.closed = true;
            this.writable = false;
            this.emit("close");
          }
          callback?.(null);
        });
      });
    }
    write(chunk, encodingOrCallback, callback) {
      if (this.writableEnded || this.destroyed) {
        const error = new Error("write after end");
        const cb2 = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
        queueMicrotask(() => cb2?.(error));
        return false;
      }
      let data;
      if (typeof chunk === "string") {
        data = import_buffer.Buffer.from(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : "utf8");
      } else if (isArrayBufferView(chunk)) {
        data = new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
      } else {
        throw createInvalidArgTypeError("chunk", "a string, Buffer, TypedArray, or DataView", chunk);
      }
      if (this.writableLength + data.length > MAX_WRITE_STREAM_BYTES) {
        const error = new Error(`WriteStream buffer exceeded ${MAX_WRITE_STREAM_BYTES} bytes`);
        this.errored = error;
        this.destroyed = true;
        this.writable = false;
        const cb2 = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
        queueMicrotask(() => {
          cb2?.(error);
          this.emit("error", error);
        });
        return false;
      }
      this._chunks.push(data);
      this.bytesWritten += data.length;
      this.writableLength += data.length;
      const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      queueMicrotask(() => cb?.(null));
      return true;
    }
    end(chunkOrCb, encodingOrCallback, callback) {
      if (this.writableEnded) {
        return this;
      }
      let cb;
      if (typeof chunkOrCb === "function") {
        cb = chunkOrCb;
      } else if (typeof encodingOrCallback === "function") {
        cb = encodingOrCallback;
        if (chunkOrCb !== void 0 && chunkOrCb !== null) {
          this.write(chunkOrCb);
        }
      } else {
        cb = callback;
        if (chunkOrCb !== void 0 && chunkOrCb !== null) {
          this.write(chunkOrCb, encodingOrCallback);
        }
      }
      this.writableEnded = true;
      this.writable = false;
      this.writableFinished = true;
      this.writableLength = 0;
      queueMicrotask(() => {
        void (async () => {
          try {
            if (this._fileHandle) {
              for (const chunk of this._chunks) {
                await this._fileHandle.write(chunk, 0, chunk.byteLength, void 0);
              }
              if (this.autoClose && !this._fileHandle.closed) {
                await this._fileHandle.close();
              }
            } else if (this.fd !== null && this.fd >= 0) {
              for (const chunk of this._chunks) {
                fs.writeSync(this.fd, chunk, 0, chunk.byteLength, null);
              }
              if (this.autoClose) {
                await this._closeUnderlying();
              }
            } else {
              const pathStr = typeof this.path === "string" ? this.path : this.path instanceof import_buffer.Buffer ? this.path.toString() : null;
              if (!pathStr) {
                throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
              }
              fs.writeFileSync(pathStr, import_buffer.Buffer.concat(this._chunks.map((chunk) => import_buffer.Buffer.from(chunk))));
            }
            this.emit("finish");
            if (this.autoClose && !this.closed) {
              this.closed = true;
              this.emit("close");
            }
            cb?.();
          } catch (error) {
            this.errored = error;
            this.emit("error", error);
          }
        })();
      });
      return this;
    }
    setDefaultEncoding(_encoding) {
      return this;
    }
    cork() {
      this.writableCorked++;
    }
    uncork() {
      if (this.writableCorked > 0) {
        this.writableCorked--;
      }
    }
    destroy(error) {
      if (this.destroyed) {
        return this;
      }
      this.destroyed = true;
      this.writable = false;
      if (error) {
        this.errored = error;
        this.emit("error", error);
      }
      queueMicrotask(() => {
        void this._closeUnderlying().then(() => {
          if (!this.closed) {
            this.closed = true;
            this.emit("close");
          }
        });
      });
      return this;
    }
    addListener(event, listener) {
      return this.on(event, listener);
    }
    on(event, listener) {
      const listeners = this._listeners.get(event) ?? [];
      listeners.push(listener);
      this._listeners.set(event, listeners);
      return this;
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.removeListener(event, wrapper);
        listener(...args);
      };
      return this.on(event, wrapper);
    }
    removeListener(event, listener) {
      const listeners = this._listeners.get(event);
      if (!listeners) {
        return this;
      }
      const index = listeners.indexOf(listener);
      if (index >= 0) {
        listeners.splice(index, 1);
      }
      return this;
    }
    off(event, listener) {
      return this.removeListener(event, listener);
    }
    removeAllListeners(event) {
      if (event === void 0) {
        this._listeners.clear();
      } else {
        this._listeners.delete(event);
      }
      return this;
    }
    emit(event, ...args) {
      const listeners = this._listeners.get(event);
      if (!listeners?.length) {
        return false;
      }
      listeners.slice().forEach((listener) => listener(...args));
      return true;
    }
    pipe(destination, _options) {
      return destination;
    }
    unpipe(_destination) {
      return this;
    }
    [Symbol.asyncDispose]() {
      return Promise.resolve();
    }
  };
  var ReadStreamClass = ReadStream;
  var WriteStreamClass = WriteStream;
  var ReadStreamFactory = function ReadStream2(path, options) {
    validateEncodingOption(options);
    return new ReadStreamClass(path, options);
  };
  ReadStreamFactory.prototype = ReadStream.prototype;
  var WriteStreamFactory = function WriteStream2(path, options) {
    validateEncodingOption(options);
    validateWriteStreamStartOption(options ?? {});
    return new WriteStreamClass(path, options);
  };
  WriteStreamFactory.prototype = WriteStream.prototype;
  function parseFlags(flags) {
    if (typeof flags === "number") return flags;
    const flagMap = {
      r: O_RDONLY,
      "r+": O_RDWR,
      rs: O_RDONLY,
      "rs+": O_RDWR,
      w: O_WRONLY | O_CREAT | O_TRUNC,
      "w+": O_RDWR | O_CREAT | O_TRUNC,
      a: O_WRONLY | O_APPEND | O_CREAT,
      "a+": O_RDWR | O_APPEND | O_CREAT,
      wx: O_WRONLY | O_CREAT | O_TRUNC | O_EXCL,
      xw: O_WRONLY | O_CREAT | O_TRUNC | O_EXCL,
      "wx+": O_RDWR | O_CREAT | O_TRUNC | O_EXCL,
      "xw+": O_RDWR | O_CREAT | O_TRUNC | O_EXCL,
      ax: O_WRONLY | O_APPEND | O_CREAT | O_EXCL,
      xa: O_WRONLY | O_APPEND | O_CREAT | O_EXCL,
      "ax+": O_RDWR | O_APPEND | O_CREAT | O_EXCL,
      "xa+": O_RDWR | O_APPEND | O_CREAT | O_EXCL
    };
    if (flags in flagMap) return flagMap[flags];
    throw new Error("Unknown file flag: " + flags);
  }
  function createFsError(code, message, syscall, path) {
    const err = new Error(message);
    err.code = code;
    err.errno = code === "ENOENT" ? -2 : code === "EACCES" ? -13 : code === "EBADF" ? -9 : code === "EMFILE" ? -24 : -1;
    err.syscall = syscall;
    if (path) err.path = path;
    return err;
  }
  function bridgeCall(fn, syscall, path) {
    try {
      return fn();
    } catch (err) {
      const msg = err.message || String(err);
      if (msg.includes("ENOENT") || msg.includes("no such file or directory") || msg.includes("not found")) {
        throw createFsError("ENOENT", `ENOENT: no such file or directory, ${syscall} '${path}'`, syscall, path);
      }
      if (msg.includes("EACCES") || msg.includes("permission denied")) {
        throw createFsError("EACCES", `EACCES: permission denied, ${syscall} '${path}'`, syscall, path);
      }
      if (msg.includes("EEXIST") || msg.includes("file already exists")) {
        throw createFsError("EEXIST", `EEXIST: file already exists, ${syscall} '${path}'`, syscall, path);
      }
      if (msg.includes("EINVAL") || msg.includes("invalid argument")) {
        throw createFsError("EINVAL", `EINVAL: invalid argument, ${syscall} '${path}'`, syscall, path);
      }
      throw err;
    }
  }
  function _globToRegex(pattern) {
    let regexStr = "";
    let i = 0;
    while (i < pattern.length) {
      const ch = pattern[i];
      if (ch === "*" && pattern[i + 1] === "*") {
        if (pattern[i + 2] === "/") {
          regexStr += "(?:.+/)?";
          i += 3;
        } else {
          regexStr += ".*";
          i += 2;
        }
      } else if (ch === "*") {
        regexStr += "[^/]*";
        i++;
      } else if (ch === "?") {
        regexStr += "[^/]";
        i++;
      } else if (ch === "{") {
        const close = pattern.indexOf("}", i);
        if (close !== -1) {
          const alternatives = pattern.slice(i + 1, close).split(",");
          regexStr += "(?:" + alternatives.map((a) => a.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\\\*/g, "[^/]*")).join("|") + ")";
          i = close + 1;
        } else {
          regexStr += "\\{";
          i++;
        }
      } else if (ch === "[") {
        const close = pattern.indexOf("]", i);
        if (close !== -1) {
          regexStr += pattern.slice(i, close + 1);
          i = close + 1;
        } else {
          regexStr += "\\[";
          i++;
        }
      } else if (".+^${}()|[]\\".includes(ch)) {
        regexStr += "\\" + ch;
        i++;
      } else {
        regexStr += ch;
        i++;
      }
    }
    return new RegExp("^" + regexStr + "$");
  }
  function _globGetBase(pattern) {
    const parts = pattern.split("/");
    const baseParts = [];
    for (const part of parts) {
      if (/[*?{}\[\]]/.test(part)) break;
      baseParts.push(part);
    }
    return baseParts.join("/") || "/";
  }
  var MAX_GLOB_DEPTH = 100;
  function _globCollect(pattern, results) {
    const regex = _globToRegex(pattern);
    const base = _globGetBase(pattern);
    const walk = (dir, depth) => {
      if (depth > MAX_GLOB_DEPTH) return;
      let entries;
      try {
        entries = _globReadDir(dir);
      } catch {
        return;
      }
      for (const entry of entries) {
        const fullPath = dir === "/" ? "/" + entry : dir + "/" + entry;
        if (regex.test(fullPath)) {
          results.push(fullPath);
        }
        try {
          const stat = _globStat(fullPath);
          if (stat.isDirectory()) {
            walk(fullPath, depth + 1);
          }
        } catch {
        }
      }
    };
    try {
      if (regex.test(base)) {
        const stat = _globStat(base);
        if (!stat.isDirectory()) {
          results.push(base);
          return;
        }
      }
      walk(base, 0);
    } catch {
    }
  }
  var _globReadDir;
  var _globStat;
  function toPathString(path) {
    return normalizePathLike(path);
  }
  function getBridgeSyncFn(name) {
    return typeof globalThis !== "undefined" ? globalThis[name] : void 0;
  }
  function createBridgeSyncFacade(name) {
    return {
      applySync(_thisArg, args) {
        const fn = getBridgeSyncFn(name);
        if (typeof fn === "function") {
          return fn(...(args || []));
        }
        if (fn && typeof fn.applySync === "function") {
          return fn.applySync(_thisArg, args);
        }
        return void 0;
      },
      applySyncPromise(_thisArg, args) {
        const fn = getBridgeSyncFn(name);
        if (typeof fn === "function") {
          return fn(...(args || []));
        }
        if (fn && typeof fn.applySync === "function") {
          return fn.applySync(_thisArg, args);
        }
        if (fn && typeof fn.applySyncPromise === "function") {
          return fn.applySyncPromise(_thisArg, args);
        }
        return void 0;
      }
    };
  }
  var _fs = {
    readFile: createBridgeSyncFacade("_fsReadFile"),
    writeFile: createBridgeSyncFacade("_fsWriteFile"),
    readFileBinary: createBridgeSyncFacade("_fsReadFileBinary"),
    writeFileBinary: createBridgeSyncFacade("_fsWriteFileBinary"),
    readDir: createBridgeSyncFacade("_fsReadDir"),
    mkdir: createBridgeSyncFacade("_fsMkdir"),
    rmdir: createBridgeSyncFacade("_fsRmdir"),
    exists: createBridgeSyncFacade("_fsExists"),
    stat: createBridgeSyncFacade("_fsStat"),
    unlink: createBridgeSyncFacade("_fsUnlink"),
    rename: createBridgeSyncFacade("_fsRename"),
    chmod: createBridgeSyncFacade("_fsChmod"),
    chown: createBridgeSyncFacade("_fsChown"),
    link: createBridgeSyncFacade("_fsLink"),
    symlink: createBridgeSyncFacade("_fsSymlink"),
    readlink: createBridgeSyncFacade("_fsReadlink"),
    lstat: createBridgeSyncFacade("_fsLstat"),
    truncate: createBridgeSyncFacade("_fsTruncate"),
    utimes: createBridgeSyncFacade("_fsUtimes")
  };
  var _fdOpen = createBridgeSyncFacade("fs.openSync");
  var _fdClose = createBridgeSyncFacade("fs.closeSync");
  var _fdRead = createBridgeSyncFacade("fs.readSync");
  var _fdWrite = createBridgeSyncFacade("fs.writeSync");
  var _fdFstat = createBridgeSyncFacade("fs.fstatSync");
  var _fdFtruncate = createBridgeSyncFacade("fs.ftruncateSync");
  var _fdFsync = createBridgeSyncFacade("fs.fsyncSync");
  var _fdGetPath = createBridgeSyncFacade("fs._getPathSync");
  function decodeBridgeJson(value) {
    return typeof value === "string" ? JSON.parse(value) : value;
  }
  function throwNormalizedFsBridgeError(err, syscall, path) {
    const errMsg = err?.message || String(err);
    if (errMsg.includes("entry not found") || errMsg.includes("not found") || errMsg.includes("ENOENT") || errMsg.includes("no such file or directory")) {
      throw createFsError("ENOENT", `ENOENT: no such file or directory, ${syscall} '${path}'`, syscall, path);
    }
    if (errMsg.includes("EROFS") || errMsg.includes("read-only file system")) {
      throw createFsError("EROFS", `EROFS: read-only file system, ${syscall} '${path}'`, syscall, path);
    }
    if (errMsg.includes("ERR_ACCESS_DENIED")) {
      const error = createFsError("ERR_ACCESS_DENIED", `ERR_ACCESS_DENIED: permission denied, ${syscall} '${path}'`, syscall, path);
      error.code = "ERR_ACCESS_DENIED";
      throw error;
    }
    if (errMsg.includes("EACCES") || errMsg.includes("permission denied")) {
      throw createFsError("EACCES", `EACCES: permission denied, ${syscall} '${path}'`, syscall, path);
    }
    throw err;
  }
  function joinDirEntryPath(dirPath, entryName) {
    if (dirPath === "/") {
      return `/${entryName}`;
    }
    return dirPath.endsWith("/") ? `${dirPath}${entryName}` : `${dirPath}/${entryName}`;
  }
  function normalizeReaddirEntries(entries, dirPath, withFileTypes) {
    if (!Array.isArray(entries)) {
      return [];
    }
    if (!withFileTypes) {
      return entries.map((entry) => typeof entry === "string" ? entry : entry?.name);
    }
    return entries.map((entry) => {
      if (typeof entry === "string") {
        const stat = fs.statSync(joinDirEntryPath(dirPath, entry));
        return new Dirent(entry, stat.isDirectory(), dirPath);
      }
      return new Dirent(entry.name, entry.isDirectory, dirPath);
    });
  }
  var fs = {
    // Constants
    constants: {
      // File Access Constants
      F_OK: 0,
      R_OK: 4,
      W_OK: 2,
      X_OK: 1,
      // File Copy Constants
      COPYFILE_EXCL: 1,
      COPYFILE_FICLONE: 2,
      COPYFILE_FICLONE_FORCE: 4,
      // File Open Constants
      O_RDONLY,
      O_WRONLY,
      O_RDWR,
      O_CREAT,
      O_EXCL,
      O_NOCTTY: 256,
      O_TRUNC,
      O_APPEND,
      O_DIRECTORY: 65536,
      O_NOATIME: 262144,
      O_NOFOLLOW: 131072,
      O_SYNC: 1052672,
      O_DSYNC: 4096,
      O_SYMLINK: 2097152,
      O_DIRECT: 16384,
      O_NONBLOCK: 2048,
      // File Type Constants
      S_IFMT: 61440,
      S_IFREG: 32768,
      S_IFDIR: 16384,
      S_IFCHR: 8192,
      S_IFBLK: 24576,
      S_IFIFO: 4096,
      S_IFLNK: 40960,
      S_IFSOCK: 49152,
      // File Mode Constants
      S_IRWXU: 448,
      S_IRUSR: 256,
      S_IWUSR: 128,
      S_IXUSR: 64,
      S_IRWXG: 56,
      S_IRGRP: 32,
      S_IWGRP: 16,
      S_IXGRP: 8,
      S_IRWXO: 7,
      S_IROTH: 4,
      S_IWOTH: 2,
      S_IXOTH: 1,
      UV_FS_O_FILEMAP: 536870912
    },
    Stats,
    Dirent,
    Dir,
    // Sync methods
    readFileSync(path, options) {
      validateEncodingOption(options);
      const rawPath = typeof path === "number" ? _fdGetPath.applySync(void 0, [normalizeFdInteger(path)]) : normalizePathLike(path);
      if (!rawPath) throw createFsError("EBADF", "EBADF: bad file descriptor", "read");
      const pathStr = rawPath;
      const encoding = typeof options === "string" ? options : options?.encoding;
      try {
        if (encoding) {
          const content = _fs.readFile.applySyncPromise(void 0, [pathStr, encoding]);
          return content;
        } else {
          const base64Content = _fs.readFileBinary.applySyncPromise(void 0, [pathStr]);
          return import_buffer.Buffer.from(base64Content, "base64");
        }
      } catch (err) {
        const errMsg = err.message || String(err);
        if (errMsg.includes("entry not found") || errMsg.includes("not found") || errMsg.includes("ENOENT") || errMsg.includes("no such file or directory")) {
          throw createFsError(
            "ENOENT",
            `ENOENT: no such file or directory, open '${rawPath}'`,
            "open",
            rawPath
          );
        }
        if (errMsg.includes("EACCES") || errMsg.includes("permission denied")) {
          throw createFsError(
            "EACCES",
            `EACCES: permission denied, open '${rawPath}'`,
            "open",
            rawPath
          );
        }
        throw err;
      }
    },
    writeFileSync(file, data, _options) {
      validateEncodingOption(_options);
      const rawPath = typeof file === "number" ? _fdGetPath.applySync(void 0, [normalizeFdInteger(file)]) : normalizePathLike(file);
      if (!rawPath) throw createFsError("EBADF", "EBADF: bad file descriptor", "write");
      const pathStr = rawPath;
      try {
        if (typeof data === "string") {
          return _fs.writeFile.applySyncPromise(void 0, [pathStr, data]);
        } else if (ArrayBuffer.isView(data)) {
          const uint8 = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
          const base64 = import_buffer.Buffer.from(uint8).toString("base64");
          return _fs.writeFileBinary.applySyncPromise(void 0, [pathStr, base64]);
        } else {
          return _fs.writeFile.applySyncPromise(void 0, [pathStr, String(data)]);
        }
      } catch (err) {
        throwNormalizedFsBridgeError(err, "write", rawPath);
      }
    },
    appendFileSync(path, data, options) {
      validateEncodingOption(options);
      const rawPath = normalizePathLike(path);
      let existing = "";
      try {
        existing = fs.existsSync(path) ? fs.readFileSync(path, "utf8") : "";
      } catch (err) {
        throwNormalizedFsBridgeError(err, "open", rawPath);
      }
      const content = typeof data === "string" ? data : String(data);
      try {
        fs.writeFileSync(path, existing + content, options);
      } catch (err) {
        if (!err?.code) {
          throw createFsError("EACCES", `EACCES: permission denied, write '${rawPath}'`, "write", rawPath);
        }
        throwNormalizedFsBridgeError(err, "write", rawPath);
      }
    },
    readdirSync(path, options) {
      validateEncodingOption(options);
      const rawPath = normalizePathLike(path);
      const pathStr = rawPath;
      let entriesJson;
      try {
        entriesJson = _fs.readDir.applySyncPromise(void 0, [pathStr]);
      } catch (err) {
        const errMsg = err.message || String(err);
        if (errMsg.includes("entry not found") || errMsg.includes("not found")) {
          throw createFsError(
            "ENOENT",
            `ENOENT: no such file or directory, scandir '${rawPath}'`,
            "scandir",
            rawPath
          );
        }
        throw err;
      }
      const entries = decodeBridgeJson(entriesJson);
      return normalizeReaddirEntries(entries, rawPath, options?.withFileTypes);
    },
    mkdirSync(path, options) {
      const rawPath = normalizePathLike(path);
      const pathStr = rawPath;
      const recursive = typeof options === "object" ? options?.recursive ?? false : false;
      _fs.mkdir.applySyncPromise(void 0, [pathStr, recursive]);
      return recursive ? rawPath : void 0;
    },
    rmdirSync(path, _options) {
      const pathStr = normalizePathLike(path);
      _fs.rmdir.applySyncPromise(void 0, [pathStr]);
    },
    rmSync(path, options) {
      const pathStr = toPathString(path);
      const opts = options || {};
      try {
        const stats = fs.statSync(pathStr);
        if (stats.isDirectory()) {
          if (opts.recursive) {
            const entries = fs.readdirSync(pathStr);
            for (const entry of entries) {
              const entryPath = pathStr.endsWith("/") ? pathStr + entry : pathStr + "/" + entry;
              const entryStats = fs.statSync(entryPath);
              if (entryStats.isDirectory()) {
                fs.rmSync(entryPath, { recursive: true });
              } else {
                fs.unlinkSync(entryPath);
              }
            }
            fs.rmdirSync(pathStr);
          } else {
            fs.rmdirSync(pathStr);
          }
        } else {
          fs.unlinkSync(pathStr);
        }
      } catch (e) {
        if (opts.force && e.code === "ENOENT") {
          return;
        }
        throw e;
      }
    },
    existsSync(path) {
      const pathStr = tryNormalizeExistsPath(path);
      if (!pathStr) {
        return false;
      }
      return _fs.exists.applySyncPromise(void 0, [pathStr]);
    },
    statSync(path, _options) {
      const rawPath = normalizePathLike(path);
      const pathStr = rawPath;
      let statJson;
      try {
        statJson = _fs.stat.applySyncPromise(void 0, [pathStr]);
      } catch (err) {
        const errMsg = err.message || String(err);
        if (errMsg.includes("entry not found") || errMsg.includes("not found") || errMsg.includes("ENOENT") || errMsg.includes("no such file or directory")) {
          throw createFsError(
            "ENOENT",
            `ENOENT: no such file or directory, stat '${rawPath}'`,
            "stat",
            rawPath
          );
        }
        throw err;
      }
      const stat = decodeBridgeJson(statJson);
      return new Stats(stat);
    },
    lstatSync(path, _options) {
      const pathStr = normalizePathLike(path);
      const statJson = bridgeCall(() => _fs.lstat.applySyncPromise(void 0, [pathStr]), "lstat", pathStr);
      const stat = decodeBridgeJson(statJson);
      return new Stats(stat);
    },
    unlinkSync(path) {
      const pathStr = normalizePathLike(path);
      _fs.unlink.applySyncPromise(void 0, [pathStr]);
    },
    renameSync(oldPath, newPath) {
      const oldPathStr = normalizePathLike(oldPath, "oldPath");
      const newPathStr = normalizePathLike(newPath, "newPath");
      _fs.rename.applySyncPromise(void 0, [oldPathStr, newPathStr]);
    },
    copyFileSync(src, dest, _mode) {
      const content = fs.readFileSync(src);
      fs.writeFileSync(dest, content);
    },
    // Recursive copy
    cpSync(src, dest, options) {
      const srcPath = toPathString(src);
      const destPath = toPathString(dest);
      const opts = options || {};
      const srcStat = fs.statSync(srcPath);
      if (srcStat.isDirectory()) {
        if (!opts.recursive) {
          throw createFsError(
            "ERR_FS_EISDIR",
            `Path is a directory: cp '${srcPath}'`,
            "cp",
            srcPath
          );
        }
        try {
          fs.mkdirSync(destPath, { recursive: true });
        } catch {
        }
        const entries = fs.readdirSync(srcPath);
        for (const entry of entries) {
          const srcEntry = srcPath.endsWith("/") ? srcPath + entry : srcPath + "/" + entry;
          const destEntry = destPath.endsWith("/") ? destPath + entry : destPath + "/" + entry;
          fs.cpSync(srcEntry, destEntry, opts);
        }
      } else {
        if (opts.errorOnExist && fs.existsSync(destPath)) {
          throw createFsError(
            "EEXIST",
            `EEXIST: file already exists, cp '${srcPath}' -> '${destPath}'`,
            "cp",
            destPath
          );
        }
        if (!opts.force && opts.force !== void 0 && fs.existsSync(destPath)) {
          return;
        }
        fs.copyFileSync(srcPath, destPath);
      }
    },
    // Temp directory creation
    mkdtempSync(prefix, _options) {
      validateEncodingOption(_options);
      const suffix = Math.random().toString(36).slice(2, 8);
      const dirPath = prefix + suffix;
      fs.mkdirSync(dirPath, { recursive: true });
      return dirPath;
    },
    // Directory handle (sync)
    opendirSync(path, _options) {
      const pathStr = normalizePathLike(path);
      const stat = fs.statSync(pathStr);
      if (!stat.isDirectory()) {
        throw createFsError(
          "ENOTDIR",
          `ENOTDIR: not a directory, opendir '${pathStr}'`,
          "opendir",
          pathStr
        );
      }
      return new Dir(pathStr);
    },
    // File descriptor methods
    openSync(path, flags, _mode) {
      const pathStr = normalizePathLike(path);
      const numFlags = parseFlags(flags ?? "r");
      const modeNum = normalizeOpenModeArgument(_mode);
      try {
        return _fdOpen.applySyncPromise(void 0, [pathStr, numFlags, modeNum]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("ENOENT")) throw createFsError("ENOENT", msg, "open", pathStr);
        if (msg.includes("EMFILE")) throw createFsError("EMFILE", msg, "open", pathStr);
        throw e;
      }
    },
    closeSync(fd) {
      normalizeFdInteger(fd);
      try {
        _fdClose.applySyncPromise(void 0, [fd]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, close", "close");
        throw e;
      }
    },
    readSync(fd, buffer, offset, length, position) {
      const normalized = normalizeReadSyncArgs(buffer, offset, length, position);
      let base64;
      try {
        base64 = _fdRead.applySyncPromise(void 0, [fd, normalized.length, normalized.position ?? null]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("EBADF")) throw createFsError("EBADF", msg, "read");
        throw e;
      }
      const bytes = import_buffer.Buffer.from(base64, "base64");
      const targetBuffer = new Uint8Array(
        normalized.buffer.buffer,
        normalized.buffer.byteOffset,
        normalized.buffer.byteLength
      );
      for (let i = 0; i < bytes.length && i < normalized.length; i++) {
        targetBuffer[normalized.offset + i] = bytes[i];
      }
      return bytes.length;
    },
    writeSync(fd, buffer, offsetOrPosition, lengthOrEncoding, position) {
      const normalized = normalizeWriteSyncArgs(buffer, offsetOrPosition, lengthOrEncoding, position);
      let dataBytes;
      if (typeof normalized.buffer === "string") {
        dataBytes = import_buffer.Buffer.from(normalized.buffer, normalized.encoding);
      } else {
        dataBytes = new Uint8Array(
          normalized.buffer.buffer,
          normalized.buffer.byteOffset + normalized.offset,
          normalized.length
        );
      }
      const base64 = import_buffer.Buffer.from(dataBytes).toString("base64");
      const pos = normalized.position ?? null;
      try {
        return _fdWrite.applySyncPromise(void 0, [fd, base64, pos]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("EBADF")) throw createFsError("EBADF", msg, "write");
        throw e;
      }
    },
    fstatSync(fd) {
      normalizeFdInteger(fd);
      let raw;
      try {
        raw = _fdFstat.applySyncPromise(void 0, [fd]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fstat", "fstat");
        throw e;
      }
      return new Stats(decodeBridgeJson(raw));
    },
    ftruncateSync(fd, len) {
      normalizeFdInteger(fd);
      try {
        _fdFtruncate.applySyncPromise(void 0, [fd, len]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, ftruncate", "ftruncate");
        throw e;
      }
    },
    // fsync / fdatasync — no-op for in-memory VFS (validates FD exists)
    fsyncSync(fd) {
      normalizeFdInteger(fd);
      try {
        _fdFsync.applySyncPromise(void 0, [fd]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fsync", "fsync");
        throw e;
      }
    },
    fdatasyncSync(fd) {
      normalizeFdInteger(fd);
      try {
        _fdFsync.applySyncPromise(void 0, [fd]);
      } catch (e) {
        const msg = e?.message ?? String(e);
        if (msg.includes("EBADF")) throw createFsError("EBADF", "EBADF: bad file descriptor, fdatasync", "fdatasync");
        throw e;
      }
    },
    // readv — scatter-read into multiple buffers (delegates to readSync)
    readvSync(fd, buffers, position) {
      const normalizedFd = normalizeFdInteger(fd);
      const normalizedBuffers = normalizeIoVectorBuffers(buffers);
      let totalBytesRead = 0;
      const normalizedPosition = normalizeOptionalPosition(position);
      let nextPosition = normalizedPosition;
      for (const buffer of normalizedBuffers) {
        const target = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
        const bytesRead = fs.readSync(normalizedFd, target, 0, target.byteLength, nextPosition);
        totalBytesRead += bytesRead;
        if (nextPosition !== null) {
          nextPosition += bytesRead;
        }
        if (bytesRead < target.byteLength) break;
      }
      return totalBytesRead;
    },
    // statfs — return synthetic filesystem stats for the in-memory VFS
    statfsSync(path, _options) {
      const pathStr = normalizePathLike(path);
      if (!fs.existsSync(pathStr)) {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, statfs '${pathStr}'`,
          "statfs",
          pathStr
        );
      }
      return {
        type: 16914839,
        // TMPFS_MAGIC
        bsize: 4096,
        blocks: 262144,
        // 1GB virtual capacity
        bfree: 262144,
        bavail: 262144,
        files: 1e6,
        ffree: 999999
      };
    },
    // glob — pattern matching over VFS files
    globSync(pattern, _options) {
      const patterns = Array.isArray(pattern) ? pattern : [pattern];
      const results = [];
      for (const pat of patterns) {
        _globCollect(pat, results);
      }
      return [...new Set(results)].sort();
    },
    // Metadata and link sync methods — delegate to VFS via host refs
    chmodSync(path, mode) {
      const pathStr = normalizePathLike(path);
      const modeNum = normalizeModeArgument(mode);
      bridgeCall(() => _fs.chmod.applySyncPromise(void 0, [pathStr, modeNum]), "chmod", pathStr);
    },
    chownSync(path, uid, gid) {
      const pathStr = normalizePathLike(path);
      const normalizedUid = normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
      const normalizedGid = normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
      bridgeCall(() => _fs.chown.applySyncPromise(void 0, [pathStr, normalizedUid, normalizedGid]), "chown", pathStr);
    },
    fchmodSync(fd, mode) {
      const normalizedFd = normalizeFdInteger(fd);
      const pathStr = _fdGetPath.applySync(void 0, [normalizedFd]);
      if (!pathStr) {
        throw createFsError("EBADF", "EBADF: bad file descriptor", "chmod");
      }
      fs.chmodSync(pathStr, normalizeModeArgument(mode));
    },
    fchownSync(fd, uid, gid) {
      const normalizedFd = normalizeFdInteger(fd);
      const pathStr = _fdGetPath.applySync(void 0, [normalizedFd]);
      if (!pathStr) {
        throw createFsError("EBADF", "EBADF: bad file descriptor", "chown");
      }
      fs.chownSync(pathStr, uid, gid);
    },
    lchownSync(path, uid, gid) {
      const pathStr = normalizePathLike(path);
      const normalizedUid = normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
      const normalizedGid = normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
      bridgeCall(() => _fs.chown.applySyncPromise(void 0, [pathStr, normalizedUid, normalizedGid]), "chown", pathStr);
    },
    linkSync(existingPath, newPath) {
      const existingStr = normalizePathLike(existingPath, "existingPath");
      const newStr = normalizePathLike(newPath, "newPath");
      bridgeCall(() => _fs.link.applySyncPromise(void 0, [existingStr, newStr]), "link", newStr);
    },
    symlinkSync(target, path, _type) {
      const targetStr = normalizePathLike(target, "target");
      const pathStr = normalizePathLike(path);
      bridgeCall(() => _fs.symlink.applySyncPromise(void 0, [targetStr, pathStr]), "symlink", pathStr);
    },
    readlinkSync(path, _options) {
      validateEncodingOption(_options);
      const pathStr = normalizePathLike(path);
      return bridgeCall(() => _fs.readlink.applySyncPromise(void 0, [pathStr]), "readlink", pathStr);
    },
    truncateSync(path, len) {
      const pathStr = normalizePathLike(path);
      bridgeCall(() => _fs.truncate.applySyncPromise(void 0, [pathStr, len ?? 0]), "truncate", pathStr);
    },
    utimesSync(path, atime, mtime) {
      const pathStr = normalizePathLike(path);
      const atimeNum = typeof atime === "number" ? atime : new Date(atime).getTime() / 1e3;
      const mtimeNum = typeof mtime === "number" ? mtime : new Date(mtime).getTime() / 1e3;
      bridgeCall(() => _fs.utimes.applySyncPromise(void 0, [pathStr, atimeNum, mtimeNum]), "utimes", pathStr);
    },
    // Async methods - wrap sync methods in callbacks/promises
    //
    // IMPORTANT: Low-level fd operations (open, close, read, write) and operations commonly
    // used by streaming libraries (stat, lstat, rename, unlink) must defer their callbacks
    // using queueMicrotask(). This is critical for proper stream operation.
    //
    // Why: Node.js streams (like tar, minipass, fs-minipass) use callback chains where each
    // callback triggers the next read/write operation. These streams also rely on events like
    // 'drain' to know when to resume writing. If callbacks fire synchronously, the event loop
    // never gets a chance to process these events, causing streams to stall after the first chunk.
    //
    // Example problem without queueMicrotask:
    //   1. tar calls fs.read() with callback
    //   2. Our sync implementation calls callback immediately
    //   3. Callback writes to stream, stream buffer fills, returns false (needs drain)
    //   4. Code sets up 'drain' listener and returns
    //   5. But we never returned to event loop, so 'drain' never fires
    //   6. Stream hangs forever
    //
    // With queueMicrotask, step 2 defers the callback, allowing the event loop to process
    // pending events (including 'drain') before the next operation starts.
    readFile(path, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        normalizePathLike(path);
        validateEncodingOption(options);
        try {
          callback(null, fs.readFileSync(path, options));
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.readFileSync(path, options));
      }
    },
    writeFile(path, data, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        normalizePathLike(path);
        validateEncodingOption(options);
        try {
          fs.writeFileSync(path, data, options);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(
          fs.writeFileSync(path, data, options)
        );
      }
    },
    appendFile(path, data, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        normalizePathLike(path);
        validateEncodingOption(options);
        try {
          fs.appendFileSync(path, data, options);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(
          fs.appendFileSync(path, data, options)
        );
      }
    },
    readdir(path, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        normalizePathLike(path);
        validateEncodingOption(options);
        try {
          callback(null, fs.readdirSync(path, options));
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(
          fs.readdirSync(path, options)
        );
      }
    },
    mkdir(path, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        normalizePathLike(path);
        try {
          fs.mkdirSync(path, options);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        fs.mkdirSync(path, options);
        return Promise.resolve();
      }
    },
    rmdir(path, callback) {
      if (callback) {
        normalizePathLike(path);
        const cb = callback;
        try {
          fs.rmdirSync(path);
          queueMicrotask(() => cb(null));
        } catch (e) {
          queueMicrotask(() => cb(e));
        }
      } else {
        return Promise.resolve(fs.rmdirSync(path));
      }
    },
    // rm - remove files or directories (with recursive support)
    rm(path, options, callback) {
      let opts = {};
      let cb;
      if (typeof options === "function") {
        cb = options;
      } else if (options) {
        opts = options;
        cb = callback;
      } else {
        cb = callback;
      }
      const doRm = () => {
        try {
          const stats = fs.statSync(path);
          if (stats.isDirectory()) {
            if (opts.recursive) {
              const entries = fs.readdirSync(path);
              for (const entry of entries) {
                const entryPath = path.endsWith("/") ? path + entry : path + "/" + entry;
                const entryStats = fs.statSync(entryPath);
                if (entryStats.isDirectory()) {
                  fs.rmSync(entryPath, { recursive: true });
                } else {
                  fs.unlinkSync(entryPath);
                }
              }
              fs.rmdirSync(path);
            } else {
              fs.rmdirSync(path);
            }
          } else {
            fs.unlinkSync(path);
          }
        } catch (e) {
          if (opts.force && e.code === "ENOENT") {
            return;
          }
          throw e;
        }
      };
      if (cb) {
        try {
          doRm();
          queueMicrotask(() => cb(null));
        } catch (e) {
          queueMicrotask(() => cb(e));
        }
      } else {
        doRm();
        return Promise.resolve();
      }
    },
    exists(path, callback) {
      validateCallback(callback, "cb");
      if (path === void 0) {
        throw createInvalidArgTypeError("path", "of type string or an instance of Buffer or URL", path);
      }
      queueMicrotask(() => callback(Boolean(tryNormalizeExistsPath(path) && fs.existsSync(path))));
    },
    stat(path, callback) {
      validateCallback(callback, "cb");
      normalizePathLike(path);
      const cb = callback;
      try {
        const stats = fs.statSync(path);
        queueMicrotask(() => cb(null, stats));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    },
    lstat(path, callback) {
      if (callback) {
        const cb = callback;
        try {
          const stats = fs.lstatSync(path);
          queueMicrotask(() => cb(null, stats));
        } catch (e) {
          queueMicrotask(() => cb(e));
        }
      } else {
        return Promise.resolve(fs.lstatSync(path));
      }
    },
    unlink(path, callback) {
      if (callback) {
        normalizePathLike(path);
        const cb = callback;
        try {
          fs.unlinkSync(path);
          queueMicrotask(() => cb(null));
        } catch (e) {
          queueMicrotask(() => cb(e));
        }
      } else {
        return Promise.resolve(fs.unlinkSync(path));
      }
    },
    rename(oldPath, newPath, callback) {
      if (callback) {
        normalizePathLike(oldPath, "oldPath");
        normalizePathLike(newPath, "newPath");
        const cb = callback;
        try {
          fs.renameSync(oldPath, newPath);
          queueMicrotask(() => cb(null));
        } catch (e) {
          queueMicrotask(() => cb(e));
        }
      } else {
        return Promise.resolve(fs.renameSync(oldPath, newPath));
      }
    },
    copyFile(src, dest, callback) {
      if (callback) {
        try {
          fs.copyFileSync(src, dest);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.copyFileSync(src, dest));
      }
    },
    cp(src, dest, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        try {
          fs.cpSync(src, dest, options);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.cpSync(src, dest, options));
      }
    },
    mkdtemp(prefix, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      validateCallback(callback, "cb");
      validateEncodingOption(options);
      try {
        callback(null, fs.mkdtempSync(prefix, options));
      } catch (e) {
        callback(e);
      }
    },
    opendir(path, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        try {
          callback(null, fs.opendirSync(path, options));
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.opendirSync(path, options));
      }
    },
    open(path, flags, mode, callback) {
      let resolvedFlags = "r";
      let resolvedMode = mode;
      if (typeof flags === "function") {
        callback = flags;
        resolvedMode = void 0;
      } else {
        resolvedFlags = flags ?? "r";
      }
      if (typeof mode === "function") {
        callback = mode;
        resolvedMode = void 0;
      }
      validateCallback(callback, "cb");
      normalizePathLike(path);
      normalizeOpenModeArgument(resolvedMode);
      const cb = callback;
      try {
        const fd = fs.openSync(path, resolvedFlags, resolvedMode);
        queueMicrotask(() => cb(null, fd));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    },
    close(fd, callback) {
      normalizeFdInteger(fd);
      validateCallback(callback, "cb");
      const cb = callback;
      try {
        fs.closeSync(fd);
        queueMicrotask(() => cb(null));
      } catch (e) {
        queueMicrotask(() => cb(e));
      }
    },
    read(fd, buffer, offset, length, position, callback) {
      if (callback) {
        const cb = callback;
        try {
          const bytesRead = fs.readSync(fd, buffer, offset, length, position);
          queueMicrotask(() => cb(null, bytesRead, buffer));
        } catch (e) {
          queueMicrotask(() => cb(e));
        }
      } else {
        return Promise.resolve(fs.readSync(fd, buffer, offset, length, position));
      }
    },
    write(fd, buffer, offset, length, position, callback) {
      if (typeof offset === "function") {
        callback = offset;
        offset = void 0;
        length = void 0;
        position = void 0;
      } else if (typeof length === "function") {
        callback = length;
        length = void 0;
        position = void 0;
      } else if (typeof position === "function") {
        callback = position;
        position = void 0;
      }
      if (callback) {
        const normalized = normalizeWriteSyncArgs(
          buffer,
          offset,
          length,
          position
        );
        const cb = callback;
        try {
          const bytesWritten = typeof normalized.buffer === "string" ? _fdWrite.applySyncPromise(
            void 0,
            [fd, import_buffer.Buffer.from(normalized.buffer, normalized.encoding).toString("base64"), normalized.position ?? null]
          ) : _fdWrite.applySyncPromise(
            void 0,
            [
              fd,
              import_buffer.Buffer.from(
                new Uint8Array(
                  normalized.buffer.buffer,
                  normalized.buffer.byteOffset + normalized.offset,
                  normalized.length
                )
              ).toString("base64"),
              normalized.position ?? null
            ]
          );
          queueMicrotask(() => cb(null, bytesWritten));
        } catch (e) {
          queueMicrotask(() => cb(e));
        }
      } else {
        return Promise.resolve(
          fs.writeSync(
            fd,
            buffer,
            offset,
            length,
            position
          )
        );
      }
    },
    // writev - write multiple buffers to a file descriptor
    writev(fd, buffers, position, callback) {
      if (typeof position === "function") {
        callback = position;
        position = null;
      }
      const normalizedFd = normalizeFdInteger(fd);
      const normalizedBuffers = normalizeIoVectorBuffers(buffers);
      const normalizedPosition = normalizeOptionalPosition(position);
      if (callback) {
        try {
          const bytesWritten = fs.writevSync(normalizedFd, normalizedBuffers, normalizedPosition);
          queueMicrotask(() => callback(null, bytesWritten, normalizedBuffers));
        } catch (e) {
          queueMicrotask(() => callback(e));
        }
      }
    },
    writevSync(fd, buffers, position) {
      const normalizedFd = normalizeFdInteger(fd);
      const normalizedBuffers = normalizeIoVectorBuffers(buffers);
      let nextPosition = normalizeOptionalPosition(position);
      let totalBytesWritten = 0;
      for (const buffer of normalizedBuffers) {
        const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
        totalBytesWritten += fs.writeSync(normalizedFd, bytes, 0, bytes.length, nextPosition);
        if (nextPosition !== null) {
          nextPosition += bytes.length;
        }
      }
      return totalBytesWritten;
    },
    fstat(fd, callback) {
      if (callback) {
        try {
          callback(null, fs.fstatSync(fd));
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.fstatSync(fd));
      }
    },
    // fsync / fdatasync async callback forms
    fsync(fd, callback) {
      normalizeFdInteger(fd);
      validateCallback(callback, "cb");
      try {
        fs.fsyncSync(fd);
        callback(null);
      } catch (e) {
        callback(e);
      }
    },
    fdatasync(fd, callback) {
      normalizeFdInteger(fd);
      validateCallback(callback, "cb");
      try {
        fs.fdatasyncSync(fd);
        callback(null);
      } catch (e) {
        callback(e);
      }
    },
    // readv async callback form
    readv(fd, buffers, position, callback) {
      if (typeof position === "function") {
        callback = position;
        position = null;
      }
      const normalizedFd = normalizeFdInteger(fd);
      const normalizedBuffers = normalizeIoVectorBuffers(buffers);
      const normalizedPosition = normalizeOptionalPosition(position);
      if (callback) {
        try {
          const bytesRead = fs.readvSync(normalizedFd, normalizedBuffers, normalizedPosition);
          queueMicrotask(() => callback(null, bytesRead, normalizedBuffers));
        } catch (e) {
          queueMicrotask(() => callback(e));
        }
      }
    },
    // statfs async callback form
    statfs(path, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        try {
          callback(null, fs.statfsSync(path, options));
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.statfsSync(path, options));
      }
    },
    // glob async callback form
    glob(pattern, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = void 0;
      }
      if (callback) {
        try {
          callback(null, fs.globSync(pattern, options));
        } catch (e) {
          callback(e);
        }
      }
    },
    // fs.promises API
    // Note: Using async functions to properly catch sync errors and return rejected promises
    promises: {
      async readFile(path, options) {
        if (path instanceof FileHandle) {
          return path.readFile(options);
        }
        return fs.readFileSync(path, options);
      },
      async writeFile(path, data, options) {
        if (path instanceof FileHandle) {
          return path.writeFile(data, options);
        }
        return fs.writeFileSync(path, data, options);
      },
      async appendFile(path, data, options) {
        if (path instanceof FileHandle) {
          return path.appendFile(data, options);
        }
        return fs.appendFileSync(path, data, options);
      },
      async readdir(path, options) {
        return fs.readdirSync(path, options);
      },
      async mkdir(path, options) {
        return fs.mkdirSync(path, options);
      },
      async rmdir(path) {
        return fs.rmdirSync(path);
      },
      async stat(path) {
        return fs.statSync(path);
      },
      async lstat(path) {
        return fs.lstatSync(path);
      },
      async unlink(path) {
        return fs.unlinkSync(path);
      },
      async rename(oldPath, newPath) {
        return fs.renameSync(oldPath, newPath);
      },
      async copyFile(src, dest) {
        return fs.copyFileSync(src, dest);
      },
      async cp(src, dest, options) {
        return fs.cpSync(src, dest, options);
      },
      async mkdtemp(prefix, options) {
        return fs.mkdtempSync(prefix, options);
      },
      async opendir(path, options) {
        return fs.opendirSync(path, options);
      },
      async open(path, flags, mode) {
        return new FileHandle(fs.openSync(path, flags ?? "r", mode));
      },
      async statfs(path, options) {
        return fs.statfsSync(path, options);
      },
      async glob(pattern, _options) {
        return fs.globSync(pattern, _options);
      },
      async access(path) {
        if (!fs.existsSync(path)) {
          throw createFsError(
            "ENOENT",
            `ENOENT: no such file or directory, access '${path}'`,
            "access",
            path
          );
        }
      },
      async rm(path, options) {
        return fs.rmSync(path, options);
      },
      async chmod(path, mode) {
        return fs.chmodSync(path, mode);
      },
      async chown(path, uid, gid) {
        return fs.chownSync(path, uid, gid);
      },
      async lchown(path, uid, gid) {
        return fs.lchownSync(path, uid, gid);
      },
      async link(existingPath, newPath) {
        return fs.linkSync(existingPath, newPath);
      },
      async symlink(target, path) {
        return fs.symlinkSync(target, path);
      },
      async readlink(path) {
        return fs.readlinkSync(path);
      },
      async truncate(path, len) {
        return fs.truncateSync(path, len);
      },
      async utimes(path, atime, mtime) {
        return fs.utimesSync(path, atime, mtime);
      },
      watch(path, options) {
        return createUnsupportedPromisesWatchIterator(path, options);
      }
    },
    // Compatibility methods
    accessSync(path) {
      if (!fs.existsSync(path)) {
        throw createFsError(
          "ENOENT",
          `ENOENT: no such file or directory, access '${path}'`,
          "access",
          path
        );
      }
    },
    access(path, mode, callback) {
      if (typeof mode === "function") {
        callback = mode;
        mode = void 0;
      }
      if (callback) {
        try {
          fs.accessSync(path);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return fs.promises.access(path);
      }
    },
    realpathSync: Object.assign(
      function realpathSync(path, options) {
        validateEncodingOption(options);
        const MAX_SYMLINK_DEPTH = 40;
        let symlinksFollowed = 0;
        const raw = normalizePathLike(path);
        const pending = [];
        for (const seg of raw.split("/")) {
          if (!seg || seg === ".") continue;
          if (seg === "..") {
            if (pending.length > 0) pending.pop();
          } else pending.push(seg);
        }
        const resolved = [];
        while (pending.length > 0) {
          const seg = pending.shift();
          if (seg === ".") continue;
          if (seg === "..") {
            if (resolved.length > 0) resolved.pop();
            continue;
          }
          resolved.push(seg);
          const currentPath = "/" + resolved.join("/");
          try {
            const stat = fs.lstatSync(currentPath);
            if (stat.isSymbolicLink()) {
              if (++symlinksFollowed > MAX_SYMLINK_DEPTH) {
                const err = new Error(`ELOOP: too many levels of symbolic links, realpath '${raw}'`);
                err.code = "ELOOP";
                err.syscall = "realpath";
                err.path = raw;
                throw err;
              }
              const target = fs.readlinkSync(currentPath);
              const targetSegs = target.split("/").filter(Boolean);
              if (target.startsWith("/")) {
                resolved.length = 0;
              } else {
                resolved.pop();
              }
              pending.unshift(...targetSegs);
            }
          } catch (e) {
            const err = e;
            if (err.code === "ELOOP") throw e;
            if (err.code === "ENOENT" || err.code === "ENOTDIR") {
              const enoent = new Error(`ENOENT: no such file or directory, realpath '${raw}'`);
              enoent.code = "ENOENT";
              enoent.syscall = "realpath";
              enoent.path = raw;
              throw enoent;
            }
            break;
          }
        }
        return "/" + resolved.join("/") || "/";
      },
      {
        native(path, options) {
          validateEncodingOption(options);
          return fs.realpathSync(path);
        }
      }
    ),
    realpath: Object.assign(
      function realpath(path, optionsOrCallback, callback) {
        let options;
        if (typeof optionsOrCallback === "function") {
          callback = optionsOrCallback;
        } else {
          options = optionsOrCallback;
        }
        if (callback) {
          validateEncodingOption(options);
          callback(null, fs.realpathSync(path, options));
        } else {
          return Promise.resolve(fs.realpathSync(path, options));
        }
      },
      {
        native(path, optionsOrCallback, callback) {
          let options;
          if (typeof optionsOrCallback === "function") {
            callback = optionsOrCallback;
          } else {
            options = optionsOrCallback;
          }
          if (callback) {
            validateEncodingOption(options);
            callback(null, fs.realpathSync.native(path, options));
          } else {
            return Promise.resolve(fs.realpathSync.native(path, options));
          }
        }
      }
    ),
    ReadStream: ReadStreamFactory,
    WriteStream: WriteStreamFactory,
    createReadStream: function createReadStream(path, options) {
      const opts = typeof options === "string" ? { encoding: options } : options;
      validateEncodingOption(opts);
      const fd = normalizeStreamFd(opts?.fd);
      const pathLike = normalizeStreamPath(path, fd);
      return new ReadStream(pathLike, opts);
    },
    createWriteStream: function createWriteStream(path, options) {
      const opts = typeof options === "string" ? { encoding: options } : options;
      validateEncodingOption(opts);
      validateWriteStreamStartOption(opts ?? {});
      const fd = normalizeStreamFd(opts?.fd);
      const pathLike = normalizeStreamPath(path, fd);
      return new WriteStream(pathLike, opts);
    },
    // Unsupported fs APIs — watch requires kernel-level inotify, use polling instead
    watch(..._args) {
      normalizeWatchArguments(_args[0], _args[1], _args[2]);
      throw createUnsupportedWatcherError("watch");
    },
    watchFile(..._args) {
      normalizeWatchFileArguments(_args[0], _args[1], _args[2]);
      throw createUnsupportedWatcherError("watchFile");
    },
    unwatchFile(..._args) {
      normalizePathLike(_args[0]);
      throw createUnsupportedWatcherError("unwatchFile");
    },
    chmod(path, mode, callback) {
      if (callback) {
        normalizePathLike(path);
        normalizeModeArgument(mode);
        try {
          fs.chmodSync(path, mode);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.chmodSync(path, mode));
      }
    },
    chown(path, uid, gid, callback) {
      if (callback) {
        normalizePathLike(path);
        normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
        normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
        try {
          fs.chownSync(path, uid, gid);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.chownSync(path, uid, gid));
      }
    },
    fchmod(fd, mode, callback) {
      if (callback) {
        normalizeFdInteger(fd);
        normalizeModeArgument(mode);
        try {
          fs.fchmodSync(fd, mode);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        normalizeFdInteger(fd);
        normalizeModeArgument(mode);
        return Promise.resolve(fs.fchmodSync(fd, mode));
      }
    },
    fchown(fd, uid, gid, callback) {
      if (callback) {
        normalizeFdInteger(fd);
        normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
        normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
        try {
          fs.fchownSync(fd, uid, gid);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        normalizeFdInteger(fd);
        normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
        normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
        return Promise.resolve(fs.fchownSync(fd, uid, gid));
      }
    },
    lchown(path, uid, gid, callback) {
      if (arguments.length >= 4) {
        validateCallback(callback, "cb");
        normalizePathLike(path);
        normalizeNumberArgument("uid", uid, { min: -1, max: 4294967295, allowNegativeOne: true });
        normalizeNumberArgument("gid", gid, { min: -1, max: 4294967295, allowNegativeOne: true });
        try {
          fs.lchownSync(path, uid, gid);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.lchownSync(path, uid, gid));
      }
    },
    link(existingPath, newPath, callback) {
      if (callback) {
        normalizePathLike(existingPath, "existingPath");
        normalizePathLike(newPath, "newPath");
        try {
          fs.linkSync(existingPath, newPath);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.linkSync(existingPath, newPath));
      }
    },
    symlink(target, path, typeOrCb, callback) {
      if (typeof typeOrCb === "function") {
        callback = typeOrCb;
      }
      if (callback) {
        try {
          fs.symlinkSync(target, path);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.symlinkSync(target, path));
      }
    },
    readlink(path, optionsOrCb, callback) {
      if (typeof optionsOrCb === "function") {
        callback = optionsOrCb;
        optionsOrCb = void 0;
      }
      if (callback) {
        normalizePathLike(path);
        validateEncodingOption(optionsOrCb);
        try {
          callback(null, fs.readlinkSync(path, optionsOrCb));
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.readlinkSync(path, optionsOrCb));
      }
    },
    truncate(path, lenOrCb, callback) {
      if (typeof lenOrCb === "function") {
        callback = lenOrCb;
        lenOrCb = 0;
      }
      if (callback) {
        try {
          fs.truncateSync(path, lenOrCb);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.truncateSync(path, lenOrCb));
      }
    },
    utimes(path, atime, mtime, callback) {
      if (callback) {
        try {
          fs.utimesSync(path, atime, mtime);
          callback(null);
        } catch (e) {
          callback(e);
        }
      } else {
        return Promise.resolve(fs.utimesSync(path, atime, mtime));
      }
    }
  };
  _globReadDir = (dir) => fs.readdirSync(dir);
  _globStat = (path) => fs.statSync(path);
  var fs_default = fs;
  exposeCustomGlobal("_fsModule", fs_default);

  // .agent/recovery/secure-exec/nodejs/src/bridge/os.ts
  var config = {
    platform: typeof _osConfig !== "undefined" && _osConfig.platform || "linux",
    arch: typeof _osConfig !== "undefined" && _osConfig.arch || "x64",
    type: typeof _osConfig !== "undefined" && _osConfig.type || "Linux",
    release: typeof _osConfig !== "undefined" && _osConfig.release || "5.15.0",
    version: typeof _osConfig !== "undefined" && _osConfig.version || "#1 SMP",
    homedir: typeof _osConfig !== "undefined" && _osConfig.homedir || "/root",
    tmpdir: typeof _osConfig !== "undefined" && _osConfig.tmpdir || "/tmp",
    hostname: typeof _osConfig !== "undefined" && _osConfig.hostname || "sandbox"
  };
  function getRuntimeHomeDir() {
    return globalThis.process?.env?.HOME || config.homedir;
  }
  function getRuntimeTmpDir() {
    return globalThis.process?.env?.TMPDIR || config.tmpdir;
  }
  var signals = {
    SIGHUP: 1,
    SIGINT: 2,
    SIGQUIT: 3,
    SIGILL: 4,
    SIGTRAP: 5,
    SIGABRT: 6,
    SIGIOT: 6,
    SIGBUS: 7,
    SIGFPE: 8,
    SIGKILL: 9,
    SIGUSR1: 10,
    SIGSEGV: 11,
    SIGUSR2: 12,
    SIGPIPE: 13,
    SIGALRM: 14,
    SIGTERM: 15,
    SIGSTKFLT: 16,
    SIGCHLD: 17,
    SIGCONT: 18,
    SIGSTOP: 19,
    SIGTSTP: 20,
    SIGTTIN: 21,
    SIGTTOU: 22,
    SIGURG: 23,
    SIGXCPU: 24,
    SIGXFSZ: 25,
    SIGVTALRM: 26,
    SIGPROF: 27,
    SIGWINCH: 28,
    SIGIO: 29,
    SIGPOLL: 29,
    SIGPWR: 30,
    SIGSYS: 31
  };
  var errno = {
    E2BIG: 7,
    EACCES: 13,
    EADDRINUSE: 98,
    EADDRNOTAVAIL: 99,
    EAFNOSUPPORT: 97,
    EAGAIN: 11,
    EALREADY: 114,
    EBADF: 9,
    EBADMSG: 74,
    EBUSY: 16,
    ECANCELED: 125,
    ECHILD: 10,
    ECONNABORTED: 103,
    ECONNREFUSED: 111,
    ECONNRESET: 104,
    EDEADLK: 35,
    EDESTADDRREQ: 89,
    EDOM: 33,
    EDQUOT: 122,
    EEXIST: 17,
    EFAULT: 14,
    EFBIG: 27,
    EHOSTUNREACH: 113,
    EIDRM: 43,
    EILSEQ: 84,
    EINPROGRESS: 115,
    EINTR: 4,
    EINVAL: 22,
    EIO: 5,
    EISCONN: 106,
    EISDIR: 21,
    ELOOP: 40,
    EMFILE: 24,
    EMLINK: 31,
    EMSGSIZE: 90,
    EMULTIHOP: 72,
    ENAMETOOLONG: 36,
    ENETDOWN: 100,
    ENETRESET: 102,
    ENETUNREACH: 101,
    ENFILE: 23,
    ENOBUFS: 105,
    ENODATA: 61,
    ENODEV: 19,
    ENOENT: 2,
    ENOEXEC: 8,
    ENOLCK: 37,
    ENOLINK: 67,
    ENOMEM: 12,
    ENOMSG: 42,
    ENOPROTOOPT: 92,
    ENOSPC: 28,
    ENOSR: 63,
    ENOSTR: 60,
    ENOSYS: 38,
    ENOTCONN: 107,
    ENOTDIR: 20,
    ENOTEMPTY: 39,
    ENOTSOCK: 88,
    ENOTSUP: 95,
    ENOTTY: 25,
    ENXIO: 6,
    EOPNOTSUPP: 95,
    EOVERFLOW: 75,
    EPERM: 1,
    EPIPE: 32,
    EPROTO: 71,
    EPROTONOSUPPORT: 93,
    EPROTOTYPE: 91,
    ERANGE: 34,
    EROFS: 30,
    ESPIPE: 29,
    ESRCH: 3,
    ESTALE: 116,
    ETIME: 62,
    ETIMEDOUT: 110,
    ETXTBSY: 26,
    EWOULDBLOCK: 11,
    EXDEV: 18
  };
  var priority = {
    PRIORITY_LOW: 19,
    PRIORITY_BELOW_NORMAL: 10,
    PRIORITY_NORMAL: 0,
    PRIORITY_ABOVE_NORMAL: -7,
    PRIORITY_HIGH: -14,
    PRIORITY_HIGHEST: -20
  };
  var os = {
    // Platform information
    platform() {
      return config.platform;
    },
    arch() {
      return config.arch;
    },
    type() {
      return config.type;
    },
    release() {
      return config.release;
    },
    version() {
      return config.version;
    },
    // Directory information
    homedir() {
      return getRuntimeHomeDir();
    },
    tmpdir() {
      return getRuntimeTmpDir();
    },
    // System information
    hostname() {
      return config.hostname;
    },
    // User information
    userInfo(_options) {
      return {
        username: "root",
        uid: 0,
        gid: 0,
        shell: "/bin/bash",
        homedir: getRuntimeHomeDir()
      };
    },
    // CPU information
    cpus() {
      return [
        {
          model: "Virtual CPU",
          speed: 2e3,
          times: {
            user: 1e5,
            nice: 0,
            sys: 5e4,
            idle: 8e5,
            irq: 0
          }
        }
      ];
    },
    // Memory information
    totalmem() {
      return 1073741824;
    },
    freemem() {
      return 536870912;
    },
    // System load
    loadavg() {
      return [0.1, 0.1, 0.1];
    },
    // System uptime
    uptime() {
      return 3600;
    },
    // Network interfaces (empty - not supported in sandbox)
    networkInterfaces() {
      return {};
    },
    // System endianness
    endianness() {
      return "LE";
    },
    // Line endings
    EOL: "\n",
    // Dev null path
    devNull: "/dev/null",
    // Machine type
    machine() {
      return config.arch;
    },
    // Constants (partial — Linux subset, no Windows WSA* or RTLD_DEEPBIND)
    constants: {
      signals,
      errno,
      priority,
      dlopen: {
        RTLD_LAZY: 1,
        RTLD_NOW: 2,
        RTLD_GLOBAL: 256,
        RTLD_LOCAL: 0
      },
      UV_UDP_REUSEADDR: 4
    },
    // Priority getters/setters (stubs)
    getPriority(_pid) {
      return 0;
    },
    setPriority(pid, priority2) {
      void pid;
      void priority2;
    },
    // Parallelism hint
    availableParallelism() {
      return 1;
    }
  };
  exposeCustomGlobal("_osModule", os);
  var os_default = os;

  // .agent/recovery/secure-exec/nodejs/src/bridge/child-process.ts
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
  function routeChildProcessEvent(sessionId, type, data) {
    const child = childProcessInstances.get(sessionId);
    if (!child) return;
    if (type === "stdout") {
      const buf = typeof Buffer !== "undefined" ? Buffer.from(data) : data;
      child.stdout.emit("data", buf);
    } else if (type === "stderr") {
      const buf = typeof Buffer !== "undefined" ? Buffer.from(data) : data;
      child.stderr.emit("data", buf);
    } else if (type === "exit") {
      child.exitCode = data;
      child.stdout.emit("end");
      child.stderr.emit("end");
      child.emit("close", data, null);
      child.emit("exit", data, null);
      childProcessInstances.delete(sessionId);
      if (typeof _unregisterHandle === "function") {
        _unregisterHandle(`child:${sessionId}`);
      }
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
      const encoded = typeof payload?.dataBase64 === "string" ? payload.dataBase64 : typeof payload?.data === "string" ? payload.data : payload?.data?.__agentOsType === "bytes" && typeof payload?.data?.base64 === "string" ? payload.data.base64 : "";
      const bytes = typeof Buffer !== "undefined" ? Buffer.from(encoded, "base64") : new Uint8Array(
        atob(encoded).split("").map((char) => char.charCodeAt(0))
      );
      routeChildProcessEvent(
        sessionId,
        eventTypeOrSessionId === "child_stdout" ? "stdout" : "stderr",
        bytes
      );
      return;
    }
    if (eventTypeOrSessionId === "child_exit") {
      const code = typeof payload?.code === "number" ? payload.code : Number(payload?.code ?? 1);
      routeChildProcessEvent(sessionId, "exit", code);
    }
  };
  exposeCustomGlobal("_childProcessDispatch", childProcessDispatch);
  function scheduleChildProcessPoll(sessionId, delayMs = 0) {
    const child = childProcessInstances.get(sessionId);
    if (!child || typeof _childProcessPoll === "undefined" || child._pollScheduled) {
      return;
    }
    child._pollScheduled = true;
    setTimeout(() => {
      child._pollScheduled = false;
      if (!childProcessInstances.has(sessionId)) {
        return;
      }
      const next = _childProcessPoll.applySync(void 0, [sessionId, 10]);
      if (!next || typeof next !== "object") {
        scheduleChildProcessPoll(sessionId, 5);
        return;
      }
      if (next.type === "stdout" || next.type === "stderr") {
        const payload = { sessionId };
        if (typeof next.data === "string") {
          payload.data = next.data;
        } else if (next.data?.__agentOsType === "bytes" && typeof next.data.base64 === "string") {
          payload.dataBase64 = next.data.base64;
        }
        childProcessDispatch(`child_${next.type}`, payload);
        scheduleChildProcessPoll(sessionId, 0);
        return;
      }
      if (next.type === "exit") {
        childProcessDispatch("child_exit", { sessionId, code: next.exitCode });
        return;
      }
      scheduleChildProcessPoll(sessionId, 0);
    }, delayMs);
  }
  function hasOutputListeners(stream, event) {
    return (stream._listeners[event]?.length ?? 0) > 0 || (stream._onceListeners[event]?.length ?? 0) > 0;
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
      }
      if (stream._ended && stream._bufferedChunks.length === 0 && hasOutputListeners(stream, "end")) {
        stream.emit("end");
      }
    });
  }
  function checkStreamMaxListeners(stream, event) {
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
    connected = false;
    _pollScheduled = false;
    spawnfile = "";
    spawnargs = [];
    stdin;
    stdout;
    stderr;
    stdio;
    constructor() {
      this.stdin = {
        writable: true,
        write(_data) {
          return true;
        },
        end() {
          this.writable = false;
        },
        on() {
          return this;
        },
        once() {
          return this;
        },
        emit() {
          return false;
        }
      };
      this.stdout = {
        readable: true,
        isTTY: false,
        _listeners: {},
        _onceListeners: {},
        _bufferedChunks: [],
        _ended: false,
        _flushScheduled: false,
        _maxListeners: 10,
        _maxListenersWarned: /* @__PURE__ */ new Set(),
        on(event, listener) {
          if (!this._listeners[event]) this._listeners[event] = [];
          this._listeners[event].push(listener);
          checkStreamMaxListeners(this, event);
          if (event === "data" || event === "end") {
            scheduleOutputFlush(this);
          }
          return this;
        },
        once(event, listener) {
          if (!this._onceListeners[event]) this._onceListeners[event] = [];
          this._onceListeners[event].push(listener);
          checkStreamMaxListeners(this, event);
          if (event === "data" || event === "end") {
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
          if (event === "data" && !hasOutputListeners(this, "data")) {
            this._bufferedChunks.push(args[0]);
            return false;
          }
          if (event === "end") {
            this._ended = true;
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
        read() {
          return null;
        },
        setEncoding() {
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
          return dest;
        },
        pause() {
          return this;
        },
        resume() {
          return this;
        }
      };
      this.stderr = {
        readable: true,
        isTTY: false,
        _listeners: {},
        _onceListeners: {},
        _bufferedChunks: [],
        _ended: false,
        _flushScheduled: false,
        _maxListeners: 10,
        _maxListenersWarned: /* @__PURE__ */ new Set(),
        on(event, listener) {
          if (!this._listeners[event]) this._listeners[event] = [];
          this._listeners[event].push(listener);
          checkStreamMaxListeners(this, event);
          if (event === "data" || event === "end") {
            scheduleOutputFlush(this);
          }
          return this;
        },
        once(event, listener) {
          if (!this._onceListeners[event]) this._onceListeners[event] = [];
          this._onceListeners[event].push(listener);
          checkStreamMaxListeners(this, event);
          if (event === "data" || event === "end") {
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
          if (event === "data" && !hasOutputListeners(this, "data")) {
            this._bufferedChunks.push(args[0]);
            return false;
          }
          if (event === "end") {
            this._ended = true;
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
        read() {
          return null;
        },
        setEncoding() {
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
          return dest;
        },
        pause() {
          return this;
        },
        resume() {
          return this;
        }
      };
      this.stdio = [this.stdin, this.stdout, this.stderr];
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      this._checkMaxListeners(event);
      return this;
    }
    once(event, listener) {
      if (!this._onceListeners[event]) this._onceListeners[event] = [];
      this._onceListeners[event].push(listener);
      this._checkMaxListeners(event);
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
      this.killed = true;
      this.signalCode = typeof _signal === "string" ? _signal : "SIGTERM";
      return true;
    }
    ref() {
      return this;
    }
    unref() {
      return this;
    }
    disconnect() {
      this.connected = false;
    }
    _complete(stdout, stderr, code) {
      this.exitCode = code;
      if (stdout) {
        const buf = typeof Buffer !== "undefined" ? Buffer.from(stdout) : stdout;
        this.stdout.emit("data", buf);
      }
      if (stderr) {
        const buf = typeof Buffer !== "undefined" ? Buffer.from(stderr) : stderr;
        this.stderr.emit("data", buf);
      }
      this.stdout.emit("end");
      this.stderr.emit("end");
      this.emit("close", code, this.signalCode);
      this.emit("exit", code, this.signalCode);
    }
  };
  function exec(command, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = {};
    }
    const child = spawn(command, [], { ...options, shell: true });
    child.spawnargs = [command];
    child.spawnfile = command;
    const maxBuffer = options?.maxBuffer ?? 1024 * 1024;
    let stdout = "";
    let stderr = "";
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let maxBufferExceeded = false;
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
          callback(err, stdout, stderr);
        } else if (code !== 0) {
          const err = new Error("Command failed: " + command);
          err.code = code;
          err.killed = false;
          err.signal = null;
          err.cmd = command;
          err.stdout = stdout;
          err.stderr = stderr;
          callback(err, stdout, stderr);
        } else {
          callback(null, stdout, stderr);
        }
      }
    });
    child.on("error", (err) => {
      if (callback) {
        const error = err instanceof Error ? err : new Error(String(err));
        error.code = 1;
        error.stdout = stdout;
        error.stderr = stderr;
        callback(error, stdout, stderr);
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
      JSON.stringify({ cwd: effectiveCwd, env: opts.env, maxBuffer, shell: true })
    ]);
    const result = typeof jsonResult === "string" ? JSON.parse(jsonResult) : jsonResult;
    if (result.maxBufferExceeded) {
      const err = new Error("stdout maxBuffer length exceeded");
      err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
      err.stdout = result.stdout;
      err.stderr = result.stderr;
      throw err;
    }
    if (result.code !== 0) {
      const err = new Error("Command failed: " + command);
      err.status = result.code;
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
    child.spawnfile = command;
    child.spawnargs = [command, ...argsArray];
    if (typeof _childProcessSpawnStart !== "undefined") {
      let spawnResult;
      try {
        const effectiveCwd = opts.cwd ?? (typeof process !== "undefined" ? process.cwd() : "/");
        spawnResult = _childProcessSpawnStart.applySync(void 0, [
          command,
          JSON.stringify(argsArray),
          JSON.stringify({ cwd: effectiveCwd, env: opts.env, shell: opts.shell === true })
        ]);
      } catch (error) {
        const spawnError = error instanceof Error ? error : new Error(String(error));
        if (spawnError.code == null && /command not found:/i.test(String(spawnError.message || ""))) {
          spawnError.code = "ENOENT";
        }
        queueMicrotask(() => {
          child.emit("error", spawnError);
        });
        return child;
      }
      const sessionId = typeof spawnResult === "object" && spawnResult !== null ? spawnResult.childId : spawnResult;
      childProcessInstances.set(sessionId, child);
      if (typeof _registerHandle === "function") {
        _registerHandle(`child:${sessionId}`, `child_process: ${command} ${argsArray.join(" ")}`);
      }
      child.stdin.write = (data) => {
        if (typeof _childProcessStdinWrite === "undefined") return false;
        const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
        _childProcessStdinWrite.applySync(void 0, [sessionId, bytes]);
        return true;
      };
      child.stdin.end = () => {
        if (typeof _childProcessStdinClose !== "undefined") {
          _childProcessStdinClose.applySync(void 0, [sessionId]);
        }
        child.stdin.writable = false;
      };
      child.kill = (signal) => {
        if (typeof _childProcessKill === "undefined") return false;
        const sig = signal === "SIGKILL" || signal === 9 ? 9 : signal === "SIGINT" || signal === 2 ? 2 : 15;
        _childProcessKill.applySync(void 0, [sessionId, sig]);
        child.killed = true;
        child.signalCode = typeof signal === "string" ? signal : "SIGTERM";
        return true;
      };
      child.pid = typeof spawnResult === "object" && spawnResult !== null ? Number(spawnResult.pid) || -1 : Number(sessionId) || -1;
      setTimeout(() => child.emit("spawn"), 0);
      scheduleChildProcessPoll(sessionId, 0);
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
      const jsonResult = _childProcessSpawnSync.applySyncPromise(void 0, [
        command,
        JSON.stringify(argsArray),
        JSON.stringify({ cwd: effectiveCwd, env: opts.env, maxBuffer, shell: opts.shell === true })
      ]);
      const result = typeof jsonResult === "string" ? JSON.parse(jsonResult) : jsonResult;
      const stdoutBuf = typeof Buffer !== "undefined" ? Buffer.from(result.stdout) : result.stdout;
      const stderrBuf = typeof Buffer !== "undefined" ? Buffer.from(result.stderr) : result.stderr;
      if (result.maxBufferExceeded) {
        const err = new Error("stdout maxBuffer length exceeded");
        err.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
        return {
          pid: _nextChildPid++,
          output: [null, stdoutBuf, stderrBuf],
          stdout: stdoutBuf,
          stderr: stderrBuf,
          status: result.code,
          signal: null,
          error: err
        };
      }
      return {
        pid: _nextChildPid++,
        output: [null, stdoutBuf, stderrBuf],
        stdout: stdoutBuf,
        stderr: stderrBuf,
        status: result.code,
        signal: null,
        error: void 0
      };
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : String(err);
      const stderrBuf = typeof Buffer !== "undefined" ? Buffer.from(errMsg) : errMsg;
      return {
        pid: _nextChildPid++,
        output: [null, "", stderrBuf],
        stdout: typeof Buffer !== "undefined" ? Buffer.from("") : "",
        stderr: stderrBuf,
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
  function fork(_modulePath, _args, _options) {
    throw new Error("child_process.fork is not supported in sandbox");
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

  var UndiciAgent = undiciAgentModule?.default ?? undiciAgentModule;
  var undiciFetch = undiciFetchModule?.fetch ?? undiciFetchModule?.default ?? undiciFetchModule;
  var UndiciHeaders = undiciHeadersModule?.Headers ?? undiciHeadersModule?.default ?? undiciHeadersModule;
  var UndiciRequest = undiciRequestModule?.Request ?? undiciRequestModule?.default ?? undiciRequestModule;
  var UndiciResponse = undiciResponseModule?.Response ?? undiciResponseModule?.default ?? undiciResponseModule;
  var setUndiciGlobalDispatcher = undiciGlobalModule?.setGlobalDispatcher;
  if (typeof globalThis[Symbol.for("undici.globalDispatcher.1")] === "undefined" && typeof setUndiciGlobalDispatcher === "function" && typeof UndiciAgent === "function") {
    setUndiciGlobalDispatcher(new UndiciAgent());
  }

  // .agent/recovery/secure-exec/nodejs/src/bridge/network.ts
  var network_exports = {};
  __export(network_exports, {
    ClientRequest: () => ClientRequest,
    Headers: () => Headers,
    IncomingMessage: () => IncomingMessage,
    Request: () => Request,
    Response: () => Response,
    default: () => network_default,
    dns: () => dns,
    fetch: () => fetch,
    http: () => http,
    http2: () => http2,
    https: () => https
  });
  var MAX_HTTP_BODY_BYTES = 50 * 1024 * 1024;
  var _fetchHandleCounter = 0;
  function serializeFetchHeaders(headers) {
    if (!headers) {
      return {};
    }
    if (headers instanceof Headers) {
      return Object.fromEntries(headers.entries());
    }
    if (typeof UndiciHeaders === "function" && headers instanceof UndiciHeaders) {
      return Object.fromEntries(headers.entries());
    }
    if (isFlatHeaderList(headers)) {
      const normalized = {};
      for (let index = 0; index < headers.length; index += 2) {
        const key = headers[index];
        const value = headers[index + 1];
        if (key !== void 0 && value !== void 0) {
          normalized[key] = value;
        }
      }
      return normalized;
    }
    if (typeof headers.entries === "function") {
      return Object.fromEntries(headers.entries());
    }
    if (typeof headers[Symbol.iterator] === "function") {
      return Object.fromEntries(headers);
    }
    return Object.fromEntries(new Headers(headers).entries());
  }
  function createFetchHeaders(headers) {
    return new Headers(serializeFetchHeaders(headers));
  }
  function normalizeFetchRequestInit(options = {}) {
    const normalized = { ...options };
    if (Object.prototype.hasOwnProperty.call(normalized, "headers")) {
      normalized.headers = serializeFetchHeaders(normalized.headers);
    }
    return normalized;
  }
  async function fetch(input, options = {}) {
    if (typeof undiciFetch !== "function") {
      throw new Error("fetch requires undici to be configured");
    }
    let resolvedInput = input;
    let normalizedOptions = options;
    if (input instanceof Request) {
      resolvedInput = input.url;
      normalizedOptions = {
        method: input.method,
        headers: serializeFetchHeaders(input.headers),
        body: input.body,
        ...options
      };
    }
    normalizedOptions = normalizeFetchRequestInit(normalizedOptions);
    const requestLabel = typeof resolvedInput === "string" ? resolvedInput : resolvedInput?.url ? String(resolvedInput.url) : String(resolvedInput);
    const handleId = typeof _registerHandle === "function" ? `fetch:${++_fetchHandleCounter}` : null;
    if (handleId) {
      _registerHandle?.(handleId, `fetch ${requestLabel}`);
    }
    try {
      return await undiciFetch(resolvedInput, normalizedOptions);
    } finally {
      if (handleId) {
        _unregisterHandle?.(handleId);
      }
    }
  }
  var Headers = class _Headers {
    _headers = {};
    constructor(init) {
      if (init && init !== null) {
        if (init instanceof _Headers) {
          this._headers = { ...init._headers };
        } else if (Array.isArray(init)) {
          init.forEach(([key, value]) => {
            this._headers[key.toLowerCase()] = value;
          });
        } else if (typeof init === "object") {
          Object.entries(init).forEach(([key, value]) => {
            this._headers[key.toLowerCase()] = value;
          });
        }
      }
    }
    get(name) {
      return this._headers[name.toLowerCase()] || null;
    }
    set(name, value) {
      this._headers[name.toLowerCase()] = value;
    }
    has(name) {
      return name.toLowerCase() in this._headers;
    }
    delete(name) {
      delete this._headers[name.toLowerCase()];
    }
    entries() {
      return Object.entries(this._headers)[Symbol.iterator]();
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    keys() {
      return Object.keys(this._headers)[Symbol.iterator]();
    }
    values() {
      return Object.values(this._headers)[Symbol.iterator]();
    }
    append(name, value) {
      const key = name.toLowerCase();
      if (key in this._headers) {
        this._headers[key] = this._headers[key] + ", " + value;
      } else {
        this._headers[key] = value;
      }
    }
    forEach(callback) {
      Object.entries(this._headers).forEach(([k, v]) => callback(v, k, this));
    }
  };
  var Request = class _Request {
    url;
    method;
    headers;
    body;
    mode;
    credentials;
    cache;
    redirect;
    referrer;
    integrity;
    constructor(input, init = {}) {
      this.url = typeof input === "string" ? input : input.url;
      this.method = init.method || (typeof input !== "string" ? input.method : void 0) || "GET";
      this.headers = createFetchHeaders(
        init.headers || (typeof input !== "string" ? input.headers : void 0)
      );
      this.body = init.body || null;
      this.mode = init.mode || "cors";
      this.credentials = init.credentials || "same-origin";
      this.cache = init.cache || "default";
      this.redirect = init.redirect || "follow";
      this.referrer = init.referrer || "about:client";
      this.integrity = init.integrity || "";
    }
    clone() {
      return new _Request(this.url, this);
    }
  };
  var Response = class _Response {
    _body;
    status;
    statusText;
    headers;
    ok;
    type;
    url;
    redirected;
    constructor(body, init = {}) {
      this._body = body || null;
      this.status = init.status || 200;
      this.statusText = init.statusText || "OK";
      this.headers = new Headers(init.headers);
      this.ok = this.status >= 200 && this.status < 300;
      this.type = "default";
      this.url = "";
      this.redirected = false;
    }
    async text() {
      return String(this._body || "");
    }
    async json() {
      return JSON.parse(this._body || "{}");
    }
    get body() {
      const bodyStr = this._body;
      if (bodyStr === null) return null;
      return {
        getReader() {
          let consumed = false;
          return {
            async read() {
              if (consumed) return { done: true };
              consumed = true;
              const encoder = new TextEncoder();
              return { done: false, value: encoder.encode(bodyStr) };
            }
          };
        }
      };
    }
    clone() {
      return new _Response(this._body, { status: this.status, statusText: this.statusText });
    }
    static error() {
      return new _Response(null, { status: 0, statusText: "" });
    }
    static redirect(url, status = 302) {
      return new _Response(null, { status, headers: { Location: url } });
    }
  };
  var dns = {
    lookup(hostname, options, callback) {
      let cb = callback;
      if (typeof options === "function") {
        cb = options;
      }
      _networkDnsLookupRaw.apply(void 0, [hostname], { result: { promise: true } }).then((resultJson) => {
        const result = JSON.parse(resultJson);
        if (result.error) {
          const err = new Error(result.error);
          err.code = result.code || "ENOTFOUND";
          cb?.(err);
        } else {
          cb?.(null, result.address, result.family);
        }
      }).catch((err) => {
        cb?.(err);
      });
    },
    resolve(hostname, rrtype, callback) {
      let cb = callback;
      if (typeof rrtype === "function") {
        cb = rrtype;
      }
      dns.lookup(hostname, (err, address) => {
        if (err) {
          cb?.(err);
        } else {
          cb?.(null, address ? [address] : []);
        }
      });
    },
    resolve4(hostname, callback) {
      dns.resolve(hostname, "A", callback);
    },
    resolve6(hostname, callback) {
      dns.resolve(hostname, "AAAA", callback);
    },
    promises: {
      lookup(hostname, _options) {
        return new Promise((resolve, reject) => {
          dns.lookup(hostname, _options, (err, address, family) => {
            if (err) reject(err);
            else resolve({ address: address || "", family: family || 4 });
          });
        });
      },
      resolve(hostname, rrtype) {
        return new Promise((resolve, reject) => {
          dns.resolve(hostname, rrtype || "A", (err, addresses) => {
            if (err) reject(err);
            else resolve(addresses || []);
          });
        });
      }
    }
  };
  function createConnResetError(message = "socket hang up") {
    const error = new Error(message);
    error.code = "ECONNRESET";
    return error;
  }
  function createAbortError2() {
    const error = new Error("The operation was aborted");
    error.name = "AbortError";
    error.code = "ABORT_ERR";
    return error;
  }
  var _moduleGlobalAgent = null;
  var IncomingMessage = class {
    headers;
    rawHeaders;
    trailers;
    rawTrailers;
    httpVersion;
    httpVersionMajor;
    httpVersionMinor;
    method;
    url;
    statusCode;
    statusMessage;
    _body;
    _isBinary;
    _listeners;
    complete;
    aborted;
    socket;
    _bodyConsumed;
    _ended;
    _flowing;
    readable;
    readableEnded;
    readableFlowing;
    destroyed;
    _encoding;
    _closeEmitted;
    constructor(response) {
      const normalizedHeaders = {};
      if (Array.isArray(response?.headers)) {
        response.headers.forEach(([key, value]) => {
          appendNormalizedHeader(normalizedHeaders, key.toLowerCase(), value);
        });
      } else if (response?.headers) {
        Object.entries(response.headers).forEach(([key, value]) => {
          normalizedHeaders[key] = Array.isArray(value) ? [...value] : value;
        });
      }
      this.rawHeaders = Array.isArray(response?.rawHeaders) ? [...response.rawHeaders] : [];
      if (this.rawHeaders.length > 0) {
        this.headers = {};
        for (let index = 0; index < this.rawHeaders.length; index += 2) {
          const key = this.rawHeaders[index];
          const value = this.rawHeaders[index + 1];
          if (key !== void 0 && value !== void 0) {
            appendNormalizedHeader(this.headers, key.toLowerCase(), value);
          }
        }
      } else {
        this.headers = normalizedHeaders;
      }
      if (this.rawHeaders.length === 0 && this.headers && typeof this.headers === "object") {
        Object.entries(this.headers).forEach(([k, v]) => {
          if (Array.isArray(v)) {
            v.forEach((entry) => {
              this.rawHeaders.push(k, entry);
            });
            return;
          }
          this.rawHeaders.push(k, v);
        });
      }
      if (response?.trailers && typeof response.trailers === "object") {
        this.trailers = response.trailers;
        this.rawTrailers = [];
        Object.entries(response.trailers).forEach(([k, v]) => {
          this.rawTrailers.push(k, v);
        });
      } else {
        this.trailers = {};
        this.rawTrailers = [];
      }
      this.httpVersion = "1.1";
      this.httpVersionMajor = 1;
      this.httpVersionMinor = 1;
      this.method = null;
      this.url = response?.url || "";
      this.statusCode = response?.status;
      this.statusMessage = response?.statusText;
      const bodyEncodingHeader = this.headers["x-body-encoding"];
      const bodyEncoding = response?.bodyEncoding || (Array.isArray(bodyEncodingHeader) ? bodyEncodingHeader[0] : bodyEncodingHeader);
      if (bodyEncoding === "base64" && response?.body && typeof Buffer !== "undefined") {
        this._body = Buffer.from(response.body, "base64").toString("binary");
        this._isBinary = true;
      } else {
        this._body = response?.body || "";
        this._isBinary = false;
      }
      this._listeners = {};
      this.complete = false;
      this.aborted = false;
      this.socket = null;
      this._bodyConsumed = false;
      this._ended = false;
      this._flowing = false;
      this.readable = true;
      this.readableEnded = false;
      this.readableFlowing = null;
      this.destroyed = false;
      this._closeEmitted = false;
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      if (event === "data" && !this._bodyConsumed) {
        this._flowing = true;
        this.readableFlowing = true;
        Promise.resolve().then(() => {
          if (!this._bodyConsumed) {
            this._bodyConsumed = true;
            if (this._body && this._body.length > 0) {
              let buf;
              if (typeof Buffer !== "undefined") {
                buf = this._isBinary ? Buffer.from(this._body, "binary") : Buffer.from(this._body);
              } else {
                buf = this._body;
              }
              this.emit("data", buf);
            }
            Promise.resolve().then(() => {
              if (!this._ended) {
                this._ended = true;
                this.complete = true;
                this.readable = false;
                this.readableEnded = true;
                this.emit("end");
              }
            });
          }
        });
      }
      if (event === "end" && this._bodyConsumed && !this._ended) {
        Promise.resolve().then(() => {
          if (!this._ended) {
            this._ended = true;
            this.complete = true;
            this.readable = false;
            this.readableEnded = true;
            listener();
          }
        });
      }
      return this;
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener(...args);
      };
      wrapper._originalListener = listener;
      return this.on(event, wrapper);
    }
    off(event, listener) {
      if (this._listeners[event]) {
        const idx = this._listeners[event].findIndex(
          (fn) => fn === listener || fn._originalListener === listener
        );
        if (idx !== -1) this._listeners[event].splice(idx, 1);
      }
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    removeAllListeners(event) {
      if (event) {
        delete this._listeners[event];
      } else {
        this._listeners = {};
      }
      return this;
    }
    emit(event, ...args) {
      const handlers = this._listeners[event];
      if (handlers) {
        handlers.slice().forEach((fn) => fn(...args));
      }
      return handlers !== void 0 && handlers.length > 0;
    }
    setEncoding(encoding) {
      this._encoding = encoding;
      return this;
    }
    read(_size) {
      if (this._bodyConsumed) return null;
      this._bodyConsumed = true;
      let buf;
      if (typeof Buffer !== "undefined") {
        buf = this._isBinary ? Buffer.from(this._body, "binary") : Buffer.from(this._body);
      } else {
        buf = this._body;
      }
      Promise.resolve().then(() => {
        if (!this._ended) {
          this._ended = true;
          this.complete = true;
          this.readable = false;
          this.readableEnded = true;
          this.emit("end");
        }
      });
      return buf;
    }
    pipe(dest) {
      let buf;
      if (typeof Buffer !== "undefined") {
        buf = this._isBinary ? Buffer.from(this._body || "", "binary") : Buffer.from(this._body || "");
      } else {
        buf = this._body || "";
      }
      if (typeof dest.write === "function" && (typeof buf === "string" ? buf.length : buf.length) > 0) {
        dest.write(buf);
      }
      if (typeof dest.end === "function") {
        Promise.resolve().then(() => dest.end());
      }
      this._bodyConsumed = true;
      this._ended = true;
      this.complete = true;
      this.readable = false;
      this.readableEnded = true;
      return dest;
    }
    pause() {
      this._flowing = false;
      this.readableFlowing = false;
      return this;
    }
    resume() {
      this._flowing = true;
      this.readableFlowing = true;
      if (!this._bodyConsumed) {
        Promise.resolve().then(() => {
          if (!this._bodyConsumed) {
            this._bodyConsumed = true;
            if (this._body) {
              let buf;
              if (typeof Buffer !== "undefined") {
                buf = this._isBinary ? Buffer.from(this._body, "binary") : Buffer.from(this._body);
              } else {
                buf = this._body;
              }
              this.emit("data", buf);
            }
            Promise.resolve().then(() => {
              if (!this._ended) {
                this._ended = true;
                this.complete = true;
                this.readable = false;
                this.readableEnded = true;
                this.emit("end");
              }
            });
          }
        });
      }
      return this;
    }
    unpipe(_dest) {
      return this;
    }
    destroy(err) {
      this.destroyed = true;
      this.readable = false;
      if (err) this.emit("error", err);
      this._emitClose();
      return this;
    }
    _abort(err = createConnResetError("aborted")) {
      if (this.aborted) {
        return;
      }
      this.aborted = true;
      this.complete = false;
      this.destroyed = true;
      this.readable = false;
      this.readableEnded = true;
      this.emit("aborted");
      if (err) {
        this.emit("error", err);
      }
      this._emitClose();
    }
    _emitClose() {
      if (this._closeEmitted) {
        return;
      }
      this._closeEmitted = true;
      this.emit("close");
    }
    [Symbol.asyncIterator]() {
      const self = this;
      let dataEmitted = false;
      let ended = false;
      return {
        async next() {
          if (ended || self._ended) {
            return { done: true, value: void 0 };
          }
          if (!dataEmitted && !self._bodyConsumed) {
            dataEmitted = true;
            self._bodyConsumed = true;
            let buf;
            if (typeof Buffer !== "undefined") {
              buf = self._isBinary ? Buffer.from(self._body || "", "binary") : Buffer.from(self._body || "");
            } else {
              buf = self._body || "";
            }
            return { done: false, value: buf };
          }
          ended = true;
          self._ended = true;
          self.complete = true;
          self.readable = false;
          self.readableEnded = true;
          return { done: true, value: void 0 };
        },
        return() {
          ended = true;
          return Promise.resolve({ done: true, value: void 0 });
        },
        throw(err) {
          ended = true;
          self.emit("error", err);
          return Promise.resolve({ done: true, value: void 0 });
        }
      };
    }
  };
  var ClientRequest = class {
    _options;
    _callback;
    _listeners = {};
    _headers = {};
    _rawHeaderNames = /* @__PURE__ */ new Map();
    _body = "";
    _bodyBytes = 0;
    _ended = false;
    _agent;
    _hostKey;
    _socketEndListener = null;
    _socketCloseListener = null;
    _loopbackAbort;
    _response = null;
    _closeEmitted = false;
    _abortEmitted = false;
    _signalAbortHandler;
    _signalPollTimer = null;
    _skipExecute = false;
    _destroyError;
    _errorEmitted = false;
    socket;
    finished = false;
    aborted = false;
    destroyed = false;
    path;
    method;
    reusedSocket = false;
    timeoutCb;
    constructor(options, callback) {
      const normalizedMethod = validateRequestMethod(options.method);
      this._options = {
        ...options,
        method: normalizedMethod,
        path: validateRequestPath(options.path)
      };
      this._callback = callback;
      this._validateTimeoutOption();
      this._setOutgoingHeaders(options.headers);
      if (!this._headers.host) {
        this._setHeaderValue("Host", buildHostHeader(this._options));
      }
      this.path = String(this._options.path || "/");
      this.method = String(this._options.method || "GET").toUpperCase();
      const agentOpt = this._options.agent;
      if (agentOpt === false) {
        this._agent = null;
      } else if (agentOpt instanceof Agent) {
        this._agent = agentOpt;
      } else {
        this._agent = _moduleGlobalAgent;
      }
      this._hostKey = this._agent ? this._agent._getHostKey(this._options) : "";
      this._bindAbortSignal();
      if (typeof this._options.timeout === "number") {
        this.setTimeout(this._options.timeout);
      }
      Promise.resolve().then(() => this._execute());
    }
    _assignSocket(socket, reusedSocket) {
      this.socket = socket;
      this.reusedSocket = reusedSocket;
      const trackedSocket = socket;
      if (!trackedSocket._agentPermanentListenersInstalled) {
        trackedSocket._agentPermanentListenersInstalled = true;
        socket.on("error", () => {
        });
        socket.on("end", () => {
        });
      }
      this._socketEndListener = () => {
      };
      socket.on("end", this._socketEndListener);
      this._socketCloseListener = () => {
        this.destroyed = true;
        this._clearTimeout();
        this._emitClose();
      };
      socket.on("close", this._socketCloseListener);
      this._applyTimeoutToSocket(socket);
      this._emit("socket", socket);
      if (this.destroyed) {
        if (this._destroyError && !this._errorEmitted) {
          this._errorEmitted = true;
          queueMicrotask(() => {
            this._emit("error", this._destroyError);
          });
        }
        socket.destroy();
        return;
      }
      void this._dispatchWithSocket(socket);
    }
    _handleSocketError(err) {
      this._emit("error", err);
    }
    _finalizeSocket(socket, keepSocketAlive) {
      if (this._socketEndListener) {
        socket.off?.("end", this._socketEndListener);
        socket.removeListener?.("end", this._socketEndListener);
        this._socketEndListener = null;
      }
      if (this._socketCloseListener) {
        socket.off?.("close", this._socketCloseListener);
        socket.removeListener?.("close", this._socketCloseListener);
        this._socketCloseListener = null;
      }
      if (this._agent) {
        this._agent._releaseSocket(this._hostKey, socket, this._options, keepSocketAlive);
      } else if (!socket.destroyed) {
        socket.destroy();
      }
    }
    async _dispatchWithSocket(socket) {
      try {
        if (typeof _networkHttpRequestRaw === "undefined") {
          console.error("http/https request requires NetworkAdapter to be configured");
          throw new Error("http/https request requires NetworkAdapter to be configured");
        }
        const url = this._buildUrl();
        const tls = {};
        if (this._options.rejectUnauthorized !== void 0) {
          tls.rejectUnauthorized = this._options.rejectUnauthorized;
        }
        const normalizedHeaders = normalizeRequestHeaders(this._options.headers);
        const requestMethod = String(this._options.method || "GET").toUpperCase();
        const responseJson = await _networkHttpRequestRaw.apply(void 0, [url, JSON.stringify({
          method: this._options.method || "GET",
          headers: normalizedHeaders,
          body: this._body || null,
          ...tls
        })], {
          result: { promise: true }
        });
        const response = JSON.parse(responseJson);
        this.finished = true;
        this._clearTimeout();
        if (response.status === 101) {
          const res2 = new IncomingMessage(response);
          let upgradeSocket = socket;
          if (response.upgradeSocketId != null) {
            upgradeSocket = new UpgradeSocket(response.upgradeSocketId, {
              host: this._options.hostname,
              port: Number(this._options.port) || 80
            });
            upgradeSocketInstances.set(response.upgradeSocketId, upgradeSocket);
          }
          const head = typeof Buffer !== "undefined" ? response.body ? Buffer.from(response.body, "base64") : Buffer.alloc(0) : new Uint8Array(0);
          res2.socket = upgradeSocket;
          upgradeSocket.once("close", () => {
            this._emit("close");
          });
          if (this._listenerCount("upgrade") === 0) {
            process.nextTick(() => {
              this._finalizeSocket(socket, false);
            });
            upgradeSocket.destroy();
            return;
          }
          this._emit("upgrade", res2, upgradeSocket, head);
          process.nextTick(() => {
            this._finalizeSocket(socket, false);
          });
          return;
        }
        if (requestMethod === "CONNECT" && response.upgradeSocketId != null) {
          const res2 = new IncomingMessage(response);
          const connectSocket = new UpgradeSocket(response.upgradeSocketId, {
            host: this._options.hostname,
            port: Number(this._options.port) || 80
          });
          upgradeSocketInstances.set(response.upgradeSocketId, connectSocket);
          const head = typeof Buffer !== "undefined" ? response.body ? Buffer.from(response.body, "base64") : Buffer.alloc(0) : new Uint8Array(0);
          res2.socket = connectSocket;
          connectSocket.once("close", () => {
            this._emit("close");
          });
          this._emit("connect", res2, connectSocket, head);
          process.nextTick(() => {
            this._finalizeSocket(socket, false);
          });
          return;
        }
        for (const informational of response.informational || []) {
          this._emit("information", new IncomingMessage({
            headers: Object.fromEntries(informational.headers || []),
            rawHeaders: informational.rawHeaders,
            status: informational.status,
            statusText: informational.statusText
          }));
        }
        const res = new IncomingMessage(response);
        this._response = res;
        res.socket = socket;
        res.once("end", () => {
          process.nextTick(() => {
            this._finalizeSocket(socket, this._agent?.keepAlive === true && !this.aborted);
          });
        });
        if (this._callback) {
          this._callback(res);
        }
        this._emit("response", res);
        if (!this._callback && this._listenerCount("response") === 0) {
          queueMicrotask(() => {
            res.resume();
          });
        }
      } catch (err) {
        this._clearTimeout();
        this._emit("error", err);
        this._finalizeSocket(socket, false);
      }
    }
    _execute() {
      if (this._skipExecute) {
        return;
      }
      if (this._agent) {
        this._agent.addRequest(this, this._options);
        return;
      }
      const finish = (socket) => {
        if (!socket) {
          this._handleSocketError(new Error("Failed to create socket"));
          this._emitClose();
          return;
        }
        this._assignSocket(socket, false);
      };
      const createConnection = this._options.createConnection;
      if (typeof createConnection === "function") {
        const maybeSocket = createConnection(this._options, (_err, socket) => {
          finish(socket);
        });
        finish(maybeSocket);
        return;
      }
      finish(new FakeSocket({
        host: this._options.hostname || this._options.host || "localhost",
        port: Number(this._options.port) || 80
      }));
    }
    _buildUrl() {
      const opts = this._options;
      const protocol = opts.protocol || (opts.port === 443 ? "https:" : "http:");
      const host = opts.hostname || opts.host || "localhost";
      const port = opts.port ? ":" + opts.port : "";
      const path = opts.path || "/";
      return protocol + "//" + host + port + path;
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    addListener(event, listener) {
      return this.on(event, listener);
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener(...args);
      };
      wrapper.listener = listener;
      return this.on(event, wrapper);
    }
    off(event, listener) {
      if (this._listeners[event]) {
        const idx = this._listeners[event].findIndex(
          (registered) => registered === listener || registered.listener === listener
        );
        if (idx !== -1) this._listeners[event].splice(idx, 1);
      }
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    getHeader(name) {
      if (typeof name !== "string") {
        throw createTypeErrorWithCode(
          `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
          "ERR_INVALID_ARG_TYPE"
        );
      }
      return this._headers[name.toLowerCase()];
    }
    getHeaders() {
      const headers = /* @__PURE__ */ Object.create(null);
      for (const [key, value] of Object.entries(this._headers)) {
        headers[key] = Array.isArray(value) ? [...value] : value;
      }
      return headers;
    }
    getHeaderNames() {
      return Object.keys(this._headers);
    }
    getRawHeaderNames() {
      return Object.keys(this._headers).map((key) => this._rawHeaderNames.get(key) || key);
    }
    hasHeader(name) {
      if (typeof name !== "string") {
        throw createTypeErrorWithCode(
          `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
          "ERR_INVALID_ARG_TYPE"
        );
      }
      return Object.prototype.hasOwnProperty.call(this._headers, name.toLowerCase());
    }
    removeHeader(name) {
      if (typeof name !== "string") {
        throw createTypeErrorWithCode(
          `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
          "ERR_INVALID_ARG_TYPE"
        );
      }
      const lowerName = name.toLowerCase();
      delete this._headers[lowerName];
      this._rawHeaderNames.delete(lowerName);
      this._options.headers = { ...this._headers };
    }
    _emit(event, ...args) {
      if (this._listeners[event]) {
        this._listeners[event].forEach((fn) => fn(...args));
      }
    }
    _listenerCount(event) {
      return this._listeners[event]?.length || 0;
    }
    _setOutgoingHeaders(headers) {
      this._headers = {};
      this._rawHeaderNames = /* @__PURE__ */ new Map();
      if (!headers) {
        this._options.headers = {};
        return;
      }
      if (Array.isArray(headers)) {
        for (let index = 0; index < headers.length; index += 2) {
          const key = headers[index];
          const value = headers[index + 1];
          if (key !== void 0 && value !== void 0) {
            this._setHeaderValue(String(key), value);
          }
        }
        return;
      }
      Object.entries(headers).forEach(([key, value]) => {
        if (value !== void 0) {
          this._setHeaderValue(key, value);
        }
      });
    }
    _setHeaderValue(name, value) {
      const actualName = validateHeaderName(name).toLowerCase();
      validateHeaderValue(actualName, value);
      this._headers[actualName] = Array.isArray(value) ? value.map((entry) => String(entry)) : String(value);
      if (!this._rawHeaderNames.has(actualName)) {
        this._rawHeaderNames.set(actualName, name);
      }
      this._options.headers = { ...this._headers };
    }
    write(data) {
      const addedBytes = typeof Buffer !== "undefined" ? Buffer.byteLength(data) : data.length;
      if (this._bodyBytes + addedBytes > MAX_HTTP_BODY_BYTES) {
        throw new Error("ERR_HTTP_BODY_TOO_LARGE: request body exceeds " + MAX_HTTP_BODY_BYTES + " byte limit");
      }
      this._body += data;
      this._bodyBytes += addedBytes;
      return true;
    }
    end(data) {
      if (data) this.write(data);
      this._ended = true;
      return this;
    }
    abort() {
      if (this.aborted) {
        return;
      }
      this.aborted = true;
      if (!this._abortEmitted) {
        this._abortEmitted = true;
        queueMicrotask(() => {
          this._emit("abort");
        });
      }
      this._loopbackAbort?.();
      this.destroy();
    }
    destroy(err) {
      if (this.destroyed) {
        return this;
      }
      this.destroyed = true;
      this._clearTimeout();
      this._unbindAbortSignal();
      this._loopbackAbort?.();
      this._loopbackAbort = void 0;
      if (!this.socket && err && err.code === "ABORT_ERR") {
        this._skipExecute = true;
      }
      const responseStarted = this._response != null;
      const destroyError = err ?? (!this.aborted && !responseStarted ? createConnResetError() : void 0);
      this._destroyError = destroyError;
      if (this._response && !this._response.complete && !this._response.aborted) {
        this._response._abort(destroyError ?? createConnResetError("aborted"));
      }
      if (this.socket && !this.socket.destroyed) {
        if (destroyError && !this._errorEmitted) {
          this._errorEmitted = true;
          queueMicrotask(() => {
            this._emit("error", destroyError);
          });
        }
        this.socket.destroy(destroyError);
      } else {
        if (destroyError) {
          this._errorEmitted = true;
          queueMicrotask(() => {
            this._emit("error", destroyError);
          });
        }
        queueMicrotask(() => {
          this._emitClose();
        });
      }
      return this;
    }
    setTimeout(timeout, callback) {
      if (callback) {
        this.once("timeout", callback);
      }
      this.timeoutCb = () => {
        this._emit("timeout");
      };
      this._clearTimeout();
      if (timeout === 0) {
        return this;
      }
      if (!Number.isFinite(timeout) || timeout < 0) {
        throw new TypeError(`The "timeout" argument must be of type number. Received ${String(timeout)}`);
      }
      this._options.timeout = timeout;
      if (this.socket) {
        this._applyTimeoutToSocket(this.socket);
      }
      return this;
    }
    setNoDelay() {
      return this;
    }
    setSocketKeepAlive() {
      return this;
    }
    flushHeaders() {
    }
    _emitClose() {
      if (this._closeEmitted) {
        return;
      }
      this._closeEmitted = true;
      this._emit("close");
    }
    _applyTimeoutToSocket(socket) {
      const timeout = this._options.timeout;
      if (typeof timeout !== "number" || timeout === 0) {
        return;
      }
      if (!this.timeoutCb) {
        this.timeoutCb = () => {
          this._emit("timeout");
        };
      }
      socket.off?.("timeout", this.timeoutCb);
      socket.removeListener?.("timeout", this.timeoutCb);
      socket.setTimeout?.(timeout, this.timeoutCb);
    }
    _validateTimeoutOption() {
      const timeout = this._options.timeout;
      if (timeout === void 0) {
        return;
      }
      if (typeof timeout !== "number") {
        const received = timeout === null ? "null" : typeof timeout === "string" ? `type string ('${timeout}')` : `type ${typeof timeout} (${JSON.stringify(timeout)})`;
        const error = new TypeError(`The "timeout" argument must be of type number. Received ${received}`);
        error.code = "ERR_INVALID_ARG_TYPE";
        throw error;
      }
    }
    _bindAbortSignal() {
      const signal = this._options.signal;
      if (!signal) {
        return;
      }
      this._signalAbortHandler = () => {
        this.destroy(createAbortError2());
      };
      if (signal.aborted) {
        this.destroyed = true;
        this._skipExecute = true;
        queueMicrotask(() => {
          this._emit("error", createAbortError2());
          this._emitClose();
        });
        return;
      }
      if (typeof signal.addEventListener === "function") {
        signal.addEventListener("abort", this._signalAbortHandler, { once: true });
        return;
      }
      const signalWithOnAbort = signal;
      signalWithOnAbort.__secureExecPrevOnAbort__ = signalWithOnAbort.onabort ?? null;
      signalWithOnAbort.onabort = ((event) => {
        signalWithOnAbort.__secureExecPrevOnAbort__?.call(signal, event);
        this._signalAbortHandler?.();
      });
      this._startAbortSignalPoll(signal);
    }
    _unbindAbortSignal() {
      const signal = this._options.signal;
      if (!signal || !this._signalAbortHandler) {
        return;
      }
      if (this._signalPollTimer) {
        clearTimeout(this._signalPollTimer);
        this._signalPollTimer = null;
      }
      if (typeof signal.removeEventListener === "function") {
        signal.removeEventListener("abort", this._signalAbortHandler);
        this._signalAbortHandler = void 0;
        return;
      }
      const signalWithOnAbort = signal;
      if (signalWithOnAbort.onabort === this._signalAbortHandler) {
        signalWithOnAbort.onabort = signalWithOnAbort.__secureExecPrevOnAbort__ ?? null;
      } else if (signalWithOnAbort.__secureExecPrevOnAbort__ !== void 0) {
        signalWithOnAbort.onabort = signalWithOnAbort.__secureExecPrevOnAbort__ ?? null;
      }
      delete signalWithOnAbort.__secureExecPrevOnAbort__;
      this._signalAbortHandler = void 0;
    }
    _startAbortSignalPoll(signal) {
      const poll = () => {
        if (this.destroyed) {
          this._signalPollTimer = null;
          return;
        }
        if (signal.aborted) {
          this._signalPollTimer = null;
          this._signalAbortHandler?.();
          return;
        }
        this._signalPollTimer = setTimeout(poll, 5);
      };
      if (!this._signalPollTimer) {
        this._signalPollTimer = setTimeout(poll, 5);
      }
    }
    _clearTimeout() {
      if (this.socket && this.timeoutCb) {
        this.socket.off?.("timeout", this.timeoutCb);
        this.socket.removeListener?.("timeout", this.timeoutCb);
      }
      if (this.socket?.setTimeout) {
        this.socket.setTimeout(0);
      }
    }
  };
  var FakeSocket = class {
    remoteAddress;
    remotePort;
    localAddress = "127.0.0.1";
    localPort = 0;
    connecting = false;
    destroyed = false;
    writable = true;
    readable = true;
    timeout = 0;
    _listeners = {};
    _closed = false;
    _closeScheduled = false;
    _timeoutTimer = null;
    _freeTimer = null;
    constructor(options) {
      this.remoteAddress = options?.host || "127.0.0.1";
      this.remotePort = options?.port || 80;
    }
    setTimeout(ms, cb) {
      this.timeout = ms;
      if (cb) {
        this.on("timeout", cb);
      }
      if (this._timeoutTimer) {
        clearTimeout(this._timeoutTimer);
        this._timeoutTimer = null;
      }
      if (ms > 0) {
        this._timeoutTimer = setTimeout(() => {
          this.emit("timeout");
        }, ms);
      }
      return this;
    }
    setNoDelay(_noDelay) {
      return this;
    }
    setKeepAlive(_enable, _delay) {
      return this;
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener.call(this, ...args);
      };
      return this.on(event, wrapper);
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
    removeAllListeners(event) {
      if (event) {
        delete this._listeners[event];
      } else {
        this._listeners = {};
      }
      return this;
    }
    emit(event, ...args) {
      const handlers = this._listeners[event];
      if (handlers) handlers.slice().forEach((fn) => fn.call(this, ...args));
      return handlers !== void 0 && handlers.length > 0;
    }
    listenerCount(event) {
      return this._listeners[event]?.length || 0;
    }
    listeners(event) {
      return [...this._listeners[event] || []];
    }
    write(_data) {
      return true;
    }
    end() {
      if (this.destroyed || this._closed) return this;
      this.writable = false;
      queueMicrotask(() => {
        if (this.destroyed || this._closed) return;
        this.readable = false;
        this.emit("end");
        this.destroy();
      });
      return this;
    }
    destroy() {
      if (this.destroyed || this._closed) return this;
      this.destroyed = true;
      this._closed = true;
      this.writable = false;
      this.readable = false;
      if (this._timeoutTimer) {
        clearTimeout(this._timeoutTimer);
        this._timeoutTimer = null;
      }
      if (!this._closeScheduled) {
        this._closeScheduled = true;
        queueMicrotask(() => {
          this._closeScheduled = false;
          this.emit("close");
        });
      }
      return this;
    }
  };
  var DirectTunnelSocket = class {
    remoteAddress;
    remotePort;
    localAddress = "127.0.0.1";
    localPort = 0;
    connecting = false;
    destroyed = false;
    writable = true;
    readable = true;
    readyState = "open";
    bytesWritten = 0;
    _listeners = {};
    _encoding;
    _peer = null;
    _readableState = { endEmitted: false };
    _writableState = { finished: false, errorEmitted: false };
    constructor(options) {
      this.remoteAddress = options?.host || "127.0.0.1";
      this.remotePort = options?.port || 80;
    }
    _attachPeer(peer) {
      this._peer = peer;
    }
    setTimeout(_ms, _cb) {
      return this;
    }
    setNoDelay(_noDelay) {
      return this;
    }
    setKeepAlive(_enable, _delay) {
      return this;
    }
    setEncoding(encoding) {
      this._encoding = encoding;
      return this;
    }
    ref() {
      return this;
    }
    unref() {
      return this;
    }
    cork() {
    }
    uncork() {
    }
    pause() {
      return this;
    }
    resume() {
      return this;
    }
    address() {
      return { address: this.localAddress, family: "IPv4", port: this.localPort };
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener.call(this, ...args);
      };
      return this.on(event, wrapper);
    }
    off(event, listener) {
      const listeners = this._listeners[event];
      if (!listeners) return this;
      const index = listeners.indexOf(listener);
      if (index !== -1) listeners.splice(index, 1);
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    removeAllListeners(event) {
      if (event) {
        delete this._listeners[event];
      } else {
        this._listeners = {};
      }
      return this;
    }
    emit(event, ...args) {
      const listeners = this._listeners[event];
      if (!listeners || listeners.length === 0) return false;
      listeners.slice().forEach((listener) => listener.call(this, ...args));
      return true;
    }
    listenerCount(event) {
      return this._listeners[event]?.length || 0;
    }
    write(data, encodingOrCb, cb) {
      if (this.destroyed || !this._peer) return false;
      const callback = typeof encodingOrCb === "function" ? encodingOrCb : cb;
      const buffer = normalizeSocketChunk(data);
      this.bytesWritten += buffer.length;
      queueMicrotask(() => {
        this._peer?._pushData(buffer);
      });
      callback?.();
      return true;
    }
    end(data) {
      if (data !== void 0) {
        this.write(data);
      }
      this.writable = false;
      this._writableState.finished = true;
      queueMicrotask(() => {
        this._peer?._pushEnd();
      });
      this.emit("finish");
      return this;
    }
    destroy(err) {
      if (this.destroyed) return this;
      this.destroyed = true;
      this.readable = false;
      this.writable = false;
      this._readableState.endEmitted = true;
      this._writableState.finished = true;
      if (err) {
        this.emit("error", err);
      }
      queueMicrotask(() => {
        this._peer?._pushEnd();
      });
      this.emit("close", false);
      return this;
    }
    _pushData(buffer) {
      if (!this.readable || this.destroyed) {
        return;
      }
      this.emit("data", this._encoding ? buffer.toString(this._encoding) : buffer);
    }
    _pushEnd() {
      if (this.destroyed) {
        return;
      }
      this.readable = false;
      this.writable = false;
      this._readableState.endEmitted = true;
      this._writableState.finished = true;
      this.emit("end");
      this.emit("close", false);
    }
  };
  function normalizeSocketChunk(data) {
    if (typeof Buffer !== "undefined" && Buffer.isBuffer(data)) {
      return data;
    }
    if (data instanceof Uint8Array) {
      return Buffer.from(data);
    }
    return Buffer.from(String(data));
  }
  var Agent = class _Agent {
    static defaultMaxSockets = Infinity;
    maxSockets;
    maxTotalSockets;
    maxFreeSockets;
    keepAlive;
    keepAliveMsecs;
    timeout;
    requests;
    sockets;
    freeSockets;
    totalSocketCount;
    _listeners = {};
    constructor(options) {
      this._validateSocketCountOption("maxSockets", options?.maxSockets);
      this._validateSocketCountOption("maxFreeSockets", options?.maxFreeSockets);
      this._validateSocketCountOption("maxTotalSockets", options?.maxTotalSockets);
      this.keepAlive = options?.keepAlive ?? false;
      this.keepAliveMsecs = options?.keepAliveMsecs ?? 1e3;
      this.maxSockets = options?.maxSockets ?? _Agent.defaultMaxSockets;
      this.maxTotalSockets = options?.maxTotalSockets ?? Infinity;
      this.maxFreeSockets = options?.maxFreeSockets ?? 256;
      this.timeout = options?.timeout ?? -1;
      this.requests = {};
      this.sockets = {};
      this.freeSockets = {};
      this.totalSocketCount = 0;
    }
    _validateSocketCountOption(name, value) {
      if (value === void 0) return;
      if (typeof value !== "number") {
        const received = typeof value === "string" ? `type string ('${value}')` : `type ${typeof value} (${JSON.stringify(value)})`;
        const err = new TypeError(
          `The "${name}" argument must be of type number. Received ${received}`
        );
        err.code = "ERR_INVALID_ARG_TYPE";
        throw err;
      }
      if (Number.isNaN(value) || value <= 0) {
        const err = new RangeError(
          `The value of "${name}" is out of range. It must be > 0. Received ${String(value)}`
        );
        err.code = "ERR_OUT_OF_RANGE";
        throw err;
      }
    }
    getName(options) {
      const host = options?.hostname || options?.host || "localhost";
      const port = options?.port ?? "";
      const localAddress = options?.localAddress ?? "";
      let suffix = "";
      if (options?.socketPath) {
        suffix = `:${options.socketPath}`;
      } else if (options?.family === 4 || options?.family === 6) {
        suffix = `:${options.family}`;
      }
      return `${host}:${port}:${localAddress}${suffix}`;
    }
    _getHostKey(options) {
      return this.getName(options);
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener(...args);
      };
      return this.on(event, wrapper);
    }
    off(event, listener) {
      const listeners = this._listeners[event];
      if (!listeners) return this;
      const index = listeners.indexOf(listener);
      if (index !== -1) listeners.splice(index, 1);
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    emit(event, ...args) {
      const listeners = this._listeners[event];
      if (!listeners || listeners.length === 0) return false;
      listeners.slice().forEach((listener) => listener.call(this, ...args));
      return true;
    }
    createConnection(options, cb) {
      if (typeof options.createConnection === "function") {
        return options.createConnection(
          options,
          cb ?? (() => void 0)
        );
      }
      const socket = new FakeSocket({
        host: String(options.hostname || options.host || "localhost"),
        port: Number(options.port) || 80
      });
      if (cb) {
        Promise.resolve().then(() => cb(null, socket));
      }
      return socket;
    }
    addRequest(request, options) {
      const name = this.getName(options);
      const freeSocket = this._takeFreeSocket(name);
      if (freeSocket) {
        this._activateSocket(name, freeSocket);
        request._assignSocket(freeSocket, true);
        return;
      }
      if (this._canCreateSocket(name)) {
        this._createSocketForRequest(name, request, options);
        return;
      }
      if (!this.requests[name]) {
        this.requests[name] = [];
      }
      this.requests[name].push({ request, options });
    }
    _releaseSocket(name, socket, options, keepSocketAlive) {
      const removedActive = this._removeSocket(this.sockets, name, socket);
      if (keepSocketAlive && !socket.destroyed) {
        const freeList = this.freeSockets[name] ?? (this.freeSockets[name] = []);
        if (freeList.length < this.maxFreeSockets) {
          if (socket._freeTimer) {
            clearTimeout(socket._freeTimer);
            socket._freeTimer = null;
          }
          freeList.push(socket);
          if (this.timeout > 0) {
            socket._freeTimer = setTimeout(() => {
              socket._freeTimer = null;
              socket.destroy();
            }, this.timeout);
          }
          socket.emit("free");
          this.emit("free", socket, options);
        } else {
          if (removedActive) {
            this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
          }
          socket.destroy();
        }
      } else if (!socket.destroyed) {
        if (removedActive) {
          this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
        }
        socket.destroy();
      }
      Promise.resolve().then(() => this._processPendingRequests());
    }
    _removeSocketCompletely(name, socket) {
      if (socket._freeTimer) {
        clearTimeout(socket._freeTimer);
        socket._freeTimer = null;
      }
      const removed = this._removeSocket(this.sockets, name, socket) || this._removeSocket(this.freeSockets, name, socket);
      if (removed) {
        this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
        Promise.resolve().then(() => this._processPendingRequests());
      }
    }
    _canCreateSocket(name) {
      const activeCount = this.sockets[name]?.length ?? 0;
      if (activeCount >= this.maxSockets) {
        return false;
      }
      if (this.totalSocketCount < this.maxTotalSockets) {
        return true;
      }
      this._evictFreeSocket(name);
      return this.totalSocketCount < this.maxTotalSockets;
    }
    _takeFreeSocket(name) {
      const freeList = this.freeSockets[name];
      while (freeList && freeList.length > 0) {
        const socket = freeList.shift();
        if (!socket.destroyed) {
          if (socket._freeTimer) {
            clearTimeout(socket._freeTimer);
            socket._freeTimer = null;
          }
          if (freeList.length === 0) delete this.freeSockets[name];
          return socket;
        }
        this.totalSocketCount = Math.max(0, this.totalSocketCount - 1);
      }
      if (freeList && freeList.length === 0) {
        delete this.freeSockets[name];
      }
      return null;
    }
    _activateSocket(name, socket) {
      const activeList = this.sockets[name] ?? (this.sockets[name] = []);
      activeList.push(socket);
    }
    _createSocketForRequest(name, request, options) {
      let settled = false;
      const finish = (err, socket) => {
        if (settled) return;
        settled = true;
        if (err || !socket) {
          request._handleSocketError(err ?? new Error("Failed to create socket"));
          this._processPendingRequests();
          return;
        }
        if (request.destroyed) {
          this.totalSocketCount += 1;
          this._activateSocket(name, socket);
          socket.once("close", () => {
            this._removeSocketCompletely(name, socket);
          });
          request._assignSocket(socket, false);
          return;
        }
        this.totalSocketCount += 1;
        this._activateSocket(name, socket);
        socket.once("close", () => {
          this._removeSocketCompletely(name, socket);
        });
        request._assignSocket(socket, false);
      };
      const connectionOptions = {
        ...options,
        keepAlive: this.keepAlive,
        keepAliveInitialDelay: this.keepAliveMsecs
      };
      try {
        const maybeSocket = this.createConnection(connectionOptions, (err, socket) => {
          finish(err, socket);
        });
        if (maybeSocket) {
          finish(null, maybeSocket);
        }
      } catch (err) {
        finish(err instanceof Error ? err : new Error(String(err)));
      }
    }
    _processPendingRequests() {
      for (const name of Object.keys(this.requests)) {
        const queue = this.requests[name];
        while (queue && queue.length > 0) {
          const freeSocket = this._takeFreeSocket(name);
          if (freeSocket) {
            const entry2 = queue.shift();
            if (entry2.request.destroyed) {
              this._activateSocket(name, freeSocket);
              this._releaseSocket(name, freeSocket, entry2.options, true);
              continue;
            }
            this._activateSocket(name, freeSocket);
            entry2.request._assignSocket(freeSocket, true);
            continue;
          }
          if (!this._canCreateSocket(name)) {
            break;
          }
          const entry = queue.shift();
          if (entry.request.destroyed) {
            continue;
          }
          this._createSocketForRequest(name, entry.request, entry.options);
        }
        if (!queue || queue.length === 0) {
          delete this.requests[name];
        }
      }
    }
    _removeSocket(sockets, name, socket) {
      const list = sockets[name];
      if (!list) return false;
      const index = list.indexOf(socket);
      if (index === -1) return false;
      list.splice(index, 1);
      if (list.length === 0) delete sockets[name];
      return true;
    }
    _evictFreeSocket(preferredName) {
      const keys = Object.keys(this.freeSockets);
      const orderedKeys = keys.includes(preferredName) ? [...keys.filter((key) => key !== preferredName), preferredName] : keys;
      for (const key of orderedKeys) {
        const socket = this.freeSockets[key]?.[0];
        if (!socket) continue;
        socket.destroy();
        return;
      }
    }
    destroy() {
      for (const socket of Object.values(this.sockets).flat()) {
        socket.destroy();
      }
      for (const socket of Object.values(this.freeSockets).flat()) {
        socket.destroy();
      }
      this.requests = {};
      this.sockets = {};
      this.freeSockets = {};
      this.totalSocketCount = 0;
    }
  };
  function debugBridgeNetwork(...args) {
    if (process.env.SECURE_EXEC_DEBUG_HTTP_BRIDGE === "1") {
      console.error("[secure-exec bridge network]", ...args);
    }
  }
  var nextServerId = 1;
  var serverInstances = /* @__PURE__ */ new Map();
  var HTTP_METHODS = [
    "ACL",
    "BIND",
    "CHECKOUT",
    "CONNECT",
    "COPY",
    "DELETE",
    "GET",
    "HEAD",
    "LINK",
    "LOCK",
    "M-SEARCH",
    "MERGE",
    "MKACTIVITY",
    "MKCALENDAR",
    "MKCOL",
    "MOVE",
    "NOTIFY",
    "OPTIONS",
    "PATCH",
    "POST",
    "PROPFIND",
    "PROPPATCH",
    "PURGE",
    "PUT",
    "QUERY",
    "REBIND",
    "REPORT",
    "SEARCH",
    "SOURCE",
    "SUBSCRIBE",
    "TRACE",
    "UNBIND",
    "UNLINK",
    "UNLOCK",
    "UNSUBSCRIBE"
  ];
  var INVALID_REQUEST_PATH_REGEXP = /[^\u0021-\u00ff]/;
  var HTTP_TOKEN_EXTRA_CHARS = /* @__PURE__ */ new Set(["!", "#", "$", "%", "&", "'", "*", "+", "-", ".", "^", "_", "`", "|", "~"]);
  function createTypeErrorWithCode(message, code) {
    const error = new TypeError(message);
    error.code = code;
    return error;
  }
  function createErrorWithCode(message, code) {
    const error = new Error(message);
    error.code = code;
    return error;
  }
  function formatReceivedType(value) {
    if (value === null) {
      return "null";
    }
    if (Array.isArray(value)) {
      return "an instance of Array";
    }
    const valueType = typeof value;
    if (valueType === "function") {
      const name = typeof value.name === "string" && value.name.length > 0 ? value.name : "anonymous";
      return `function ${name}`;
    }
    if (valueType === "object") {
      const ctorName = value && typeof value === "object" && typeof value.constructor?.name === "string" ? value.constructor.name : "Object";
      return `an instance of ${ctorName}`;
    }
    if (valueType === "string") {
      return `type string ('${String(value)}')`;
    }
    if (valueType === "symbol") {
      return `type symbol (${String(value)})`;
    }
    return `type ${valueType} (${String(value)})`;
  }
  function createInvalidArgTypeError2(argumentName, expectedType, value) {
    return createTypeErrorWithCode(
      `The "${argumentName}" property must be of type ${expectedType}. Received ${formatReceivedType(value)}`,
      "ERR_INVALID_ARG_TYPE"
    );
  }
  function checkIsHttpToken(value) {
    if (value.length === 0) {
      return false;
    }
    for (let index = 0; index < value.length; index += 1) {
      const char = value[index];
      const code = value.charCodeAt(index);
      const isAlphaNum = code >= 48 && code <= 57 || code >= 65 && code <= 90 || code >= 97 && code <= 122;
      if (!isAlphaNum && !HTTP_TOKEN_EXTRA_CHARS.has(char)) {
        return false;
      }
    }
    return true;
  }
  function checkInvalidHeaderChar(value) {
    for (let index = 0; index < value.length; index += 1) {
      const code = value.charCodeAt(index);
      if (code === 9) {
        continue;
      }
      if (code < 32 || code === 127 || code > 255) {
        return true;
      }
    }
    return false;
  }
  function validateHeaderName(name, label = "Header name") {
    const actualName = String(name);
    if (!checkIsHttpToken(actualName)) {
      throw createTypeErrorWithCode(
        `${label} must be a valid HTTP token [${JSON.stringify(actualName)}]`,
        "ERR_INVALID_HTTP_TOKEN"
      );
    }
    return actualName;
  }
  function validateHeaderValue(name, value) {
    if (value === void 0) {
      throw createTypeErrorWithCode(
        `Invalid value "undefined" for header "${name}"`,
        "ERR_HTTP_INVALID_HEADER_VALUE"
      );
    }
    if (Array.isArray(value)) {
      for (const entry of value) {
        validateHeaderValue(name, entry);
      }
      return;
    }
    if (checkInvalidHeaderChar(String(value))) {
      throw createTypeErrorWithCode(
        `Invalid character in header content [${JSON.stringify(name)}]`,
        "ERR_INVALID_CHAR"
      );
    }
  }
  function serializeHeaderValue(value) {
    if (Array.isArray(value)) {
      return value.map((entry) => String(entry));
    }
    return String(value);
  }
  function joinHeaderValue(value) {
    return Array.isArray(value) ? value.join(", ") : value;
  }
  function cloneStoredHeaderValue(value) {
    return Array.isArray(value) ? [...value] : value;
  }
  function appendNormalizedHeader(target, key, value) {
    if (key === "set-cookie") {
      const existing2 = target[key];
      if (existing2 === void 0) {
        target[key] = [value];
      } else if (Array.isArray(existing2)) {
        existing2.push(value);
      } else {
        target[key] = [existing2, value];
      }
      return;
    }
    const existing = target[key];
    target[key] = existing === void 0 ? value : `${joinHeaderValue(existing)}, ${value}`;
  }
  function validateRequestMethod(method) {
    if (method == null || method === "") {
      return void 0;
    }
    if (typeof method !== "string") {
      throw createInvalidArgTypeError2("options.method", "string", method);
    }
    return validateHeaderName(method, "Method");
  }
  function validateRequestPath(path) {
    const resolvedPath = path == null || path === "" ? "/" : String(path);
    if (INVALID_REQUEST_PATH_REGEXP.test(resolvedPath)) {
      throw createTypeErrorWithCode(
        "Request path contains unescaped characters",
        "ERR_UNESCAPED_CHARACTERS"
      );
    }
    return resolvedPath;
  }
  function buildHostHeader(options) {
    const host = String(options.hostname || options.host || "localhost");
    const defaultPort = options.protocol === "https:" || Number(options.port) === 443 ? 443 : 80;
    const port = options.port != null ? Number(options.port) : defaultPort;
    return port === defaultPort ? host : `${host}:${port}`;
  }
  function isFlatHeaderList(headers) {
    return Array.isArray(headers) && (headers.length === 0 || typeof headers[0] === "string");
  }
  function normalizeRequestHeaders(headers) {
    if (!headers) return {};
    if (Array.isArray(headers)) {
      const normalized2 = {};
      for (let i = 0; i < headers.length; i += 2) {
        const key = headers[i];
        const value = headers[i + 1];
        if (key !== void 0 && value !== void 0) {
          const normalizedKey = validateHeaderName(key).toLowerCase();
          validateHeaderValue(normalizedKey, value);
          appendNormalizedHeader(normalized2, normalizedKey, String(value));
        }
      }
      return normalized2;
    }
    const normalized = {};
    Object.entries(headers).forEach(([key, value]) => {
      if (value === void 0) return;
      const normalizedKey = validateHeaderName(key).toLowerCase();
      validateHeaderValue(normalizedKey, value);
      if (Array.isArray(value)) {
        value.forEach((entry) => appendNormalizedHeader(normalized, normalizedKey, String(entry)));
        return;
      }
      appendNormalizedHeader(normalized, normalizedKey, String(value));
    });
    return normalized;
  }
  function hasUpgradeRequestHeaders(headers) {
    const connectionHeader = joinHeaderValue(headers.connection || "").toLowerCase();
    return connectionHeader.includes("upgrade") && Boolean(headers.upgrade);
  }
  function hasResponseBody(statusCode, method) {
    if (method === "HEAD") {
      return false;
    }
    if (statusCode >= 100 && statusCode < 200 || statusCode === 204 || statusCode === 304) {
      return false;
    }
    return true;
  }
  function splitTransferEncodingTokens(value) {
    return value.split(",").map((entry) => entry.trim().toLowerCase()).filter((entry) => entry.length > 0);
  }
  function parseContentLengthHeader(value) {
    if (value === void 0) {
      return 0;
    }
    const entries = Array.isArray(value) ? value : [value];
    let parsed = null;
    for (const entry of entries) {
      if (!/^\d+$/.test(entry)) {
        return null;
      }
      const nextValue = Number(entry);
      if (!Number.isSafeInteger(nextValue) || nextValue < 0) {
        return null;
      }
      if (parsed !== null && parsed !== nextValue) {
        return null;
      }
      parsed = nextValue;
    }
    return parsed ?? 0;
  }
  function parseChunkedBody(bodyBuffer) {
    let offset = 0;
    const chunks = [];
    while (true) {
      const lineEnd = bodyBuffer.indexOf("\r\n", offset);
      if (lineEnd === -1) {
        return { complete: false };
      }
      const sizeLine = bodyBuffer.subarray(offset, lineEnd).toString("latin1");
      if (sizeLine.length === 0 || /[\r\n]/.test(sizeLine)) {
        return null;
      }
      const [sizePart, extensionPart] = sizeLine.split(";", 2);
      if (!/^[0-9A-Fa-f]+$/.test(sizePart)) {
        return null;
      }
      if (extensionPart !== void 0 && /[\r\n]/.test(extensionPart)) {
        return null;
      }
      const chunkSize = Number.parseInt(sizePart, 16);
      if (!Number.isSafeInteger(chunkSize) || chunkSize < 0) {
        return null;
      }
      const chunkStart = lineEnd + 2;
      const chunkEnd = chunkStart + chunkSize;
      const chunkTerminatorEnd = chunkEnd + 2;
      if (chunkTerminatorEnd > bodyBuffer.length) {
        return { complete: false };
      }
      if (bodyBuffer[chunkEnd] !== 13 || bodyBuffer[chunkEnd + 1] !== 10) {
        return null;
      }
      if (chunkSize > 0) {
        chunks.push(bodyBuffer.subarray(chunkStart, chunkEnd));
        offset = chunkTerminatorEnd;
        continue;
      }
      const trailersEnd = bodyBuffer.indexOf("\r\n\r\n", chunkStart);
      if (trailersEnd === -1) {
        return { complete: false };
      }
      const trailerBlock = bodyBuffer.subarray(chunkStart, trailersEnd).toString("latin1");
      if (trailerBlock.length > 0) {
        for (const trailerLine of trailerBlock.split("\r\n")) {
          if (trailerLine.length === 0) {
            continue;
          }
          if (trailerLine.startsWith(" ") || trailerLine.startsWith("	")) {
            return null;
          }
          if (trailerLine.indexOf(":") === -1) {
            return null;
          }
        }
      }
      return {
        complete: true,
        bytesConsumed: trailersEnd + 4,
        body: chunks.length > 0 ? Buffer.concat(chunks) : Buffer.alloc(0)
      };
    }
  }
  function parseLoopbackRequestBuffer(buffer, server) {
    let requestStart = 0;
    while (requestStart + 1 < buffer.length && buffer[requestStart] === 13 && buffer[requestStart + 1] === 10) {
      requestStart += 2;
    }
    const headerEnd = buffer.indexOf("\r\n\r\n", requestStart);
    if (headerEnd === -1) {
      return { kind: "incomplete" };
    }
    const headerBlock = buffer.subarray(requestStart, headerEnd).toString("latin1");
    const [requestLine, ...headerLines] = headerBlock.split("\r\n");
    const requestMatch = /^([A-Z]+)\s+(\S+)\s+HTTP\/(1)\.(0|1)$/.exec(requestLine);
    if (!requestMatch) {
      return {
        kind: "bad-request",
        closeConnection: true
      };
    }
    const headers = {};
    const rawHeaders = [];
    let previousHeaderName = null;
    try {
      for (const headerLine of headerLines) {
        if (headerLine.length === 0) {
          continue;
        }
        if (headerLine.startsWith(" ") || headerLine.startsWith("	")) {
          return {
            kind: "bad-request",
            closeConnection: true
          };
        }
        const separatorIndex = headerLine.indexOf(":");
        if (separatorIndex === -1) {
          return {
            kind: "bad-request",
            closeConnection: true
          };
        }
        const rawName = headerLine.slice(0, separatorIndex).trim();
        const rawValue = headerLine.slice(separatorIndex + 1).trim();
        const normalizedName = validateHeaderName(rawName).toLowerCase();
        validateHeaderValue(normalizedName, rawValue);
        appendNormalizedHeader(headers, normalizedName, rawValue);
        rawHeaders.push(rawName, rawValue);
        previousHeaderName = normalizedName;
      }
    } catch {
      return {
        kind: "bad-request",
        closeConnection: true
      };
    }
    const requestMethod = requestMatch[1];
    const requestUrl = requestMatch[2];
    const httpMinorVersion = Number(requestMatch[4]);
    const requestCloseHeader = joinHeaderValue(headers.connection || "").toLowerCase();
    let closeConnection = httpMinorVersion === 0 ? !requestCloseHeader.includes("keep-alive") : requestCloseHeader.includes("close");
    if (hasUpgradeRequestHeaders(headers) && server.listenerCount("upgrade") > 0) {
      return {
        kind: "request",
        bytesConsumed: buffer.length,
        closeConnection: false,
        request: {
          method: requestMethod,
          url: requestUrl,
          headers,
          rawHeaders,
          bodyBase64: headerEnd + 4 < buffer.length ? buffer.subarray(headerEnd + 4).toString("base64") : void 0
        },
        upgradeHead: headerEnd + 4 < buffer.length ? buffer.subarray(headerEnd + 4) : Buffer.alloc(0)
      };
    }
    const transferEncoding = headers["transfer-encoding"];
    const contentLength = headers["content-length"];
    let requestBody = Buffer.alloc(0);
    let bytesConsumed = headerEnd + 4;
    if (transferEncoding !== void 0) {
      const tokens = splitTransferEncodingTokens(joinHeaderValue(transferEncoding));
      const chunkedCount = tokens.filter((entry) => entry === "chunked").length;
      const hasChunked = chunkedCount > 0;
      const chunkedIsFinal = hasChunked && tokens[tokens.length - 1] === "chunked";
      if (!hasChunked || chunkedCount !== 1 || !chunkedIsFinal || contentLength !== void 0) {
        return {
          kind: "bad-request",
          closeConnection: true
        };
      }
      const parsedChunked = parseChunkedBody(buffer.subarray(headerEnd + 4));
      if (parsedChunked === null) {
        return {
          kind: "bad-request",
          closeConnection: true
        };
      }
      if (!parsedChunked.complete) {
        return { kind: "incomplete" };
      }
      requestBody = parsedChunked.body;
      bytesConsumed = headerEnd + 4 + parsedChunked.bytesConsumed;
    } else if (contentLength !== void 0) {
      const parsedContentLength = parseContentLengthHeader(contentLength);
      if (parsedContentLength === null) {
        return {
          kind: "bad-request",
          closeConnection: true
        };
      }
      const bodyEnd = headerEnd + 4 + parsedContentLength;
      if (bodyEnd > buffer.length) {
        return { kind: "incomplete" };
      }
      requestBody = buffer.subarray(headerEnd + 4, bodyEnd);
      bytesConsumed = bodyEnd;
    }
    return {
      kind: "request",
      bytesConsumed,
      closeConnection,
      request: {
        method: requestMethod,
        url: requestUrl,
        headers,
        rawHeaders,
        bodyBase64: requestBody.length > 0 ? requestBody.toString("base64") : void 0
      }
    };
  }
  function serializeRawHeaderPairs(rawHeaders, fallbackHeaders) {
    const headers = {};
    const rawNameMap = /* @__PURE__ */ new Map();
    const order = [];
    if (Array.isArray(rawHeaders) && rawHeaders.length > 0) {
      for (let index = 0; index < rawHeaders.length; index += 2) {
        const rawName = rawHeaders[index];
        const value = rawHeaders[index + 1];
        if (rawName === void 0 || value === void 0) {
          continue;
        }
        const normalizedName = rawName.toLowerCase();
        appendNormalizedHeader(headers, normalizedName, value);
        if (!rawNameMap.has(normalizedName)) {
          rawNameMap.set(normalizedName, rawName);
          order.push(normalizedName);
        }
      }
      return { headers, rawNameMap, order };
    }
    if (Array.isArray(fallbackHeaders)) {
      for (const [name, value] of fallbackHeaders) {
        const normalizedName = name.toLowerCase();
        appendNormalizedHeader(headers, normalizedName, value);
        if (!rawNameMap.has(normalizedName)) {
          rawNameMap.set(normalizedName, name);
          order.push(normalizedName);
        }
      }
    }
    return { headers, rawNameMap, order };
  }
  function finalizeRawHeaderPairs(headers, rawNameMap, order) {
    const entries = [];
    const seen = /* @__PURE__ */ new Set();
    for (const key of order) {
      const value = headers[key];
      if (value === void 0) {
        continue;
      }
      const rawName = rawNameMap.get(key) || key;
      const serialized = Array.isArray(value) ? key === "set-cookie" ? value : [value.join(", ")] : [value];
      for (const entry of serialized) {
        entries.push([rawName, entry]);
      }
      seen.add(key);
    }
    for (const [key, value] of Object.entries(headers)) {
      if (seen.has(key)) {
        continue;
      }
      const rawName = rawNameMap.get(key) || key;
      const serialized = Array.isArray(value) ? key === "set-cookie" ? value : [value.join(", ")] : [value];
      for (const entry of serialized) {
        entries.push([rawName, entry]);
      }
    }
    return entries;
  }
  function createBadRequestResponseBuffer() {
    return Buffer.from("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n", "latin1");
  }
  function serializeLoopbackResponse(response, request, requestWantsClose) {
    const statusCode = response.status || 200;
    const statusText = HTTP_STATUS_TEXT[statusCode] || "OK";
    const {
      headers,
      rawNameMap,
      order
    } = serializeRawHeaderPairs(response.rawHeaders, response.headers);
    const trailerInfo = serializeRawHeaderPairs(response.rawTrailers, response.trailers);
    const bodyBuffer = response.body == null ? Buffer.alloc(0) : response.bodyEncoding === "base64" ? Buffer.from(response.body, "base64") : Buffer.from(response.body, "utf8");
    const bodyAllowed = hasResponseBody(statusCode, request.method);
    const transferEncodingTokens = headers["transfer-encoding"] ? splitTransferEncodingTokens(joinHeaderValue(headers["transfer-encoding"])) : [];
    const isChunked = transferEncodingTokens.includes("chunked");
    const hasExplicitContentLength = headers["content-length"] !== void 0;
    let closeConnection = requestWantsClose || response.connectionEnded === true || response.connectionReset === true;
    if (!bodyAllowed) {
      if (isChunked) {
        closeConnection = true;
      }
      delete headers["content-length"];
    } else if (!isChunked && !hasExplicitContentLength) {
      headers["content-length"] = String(bodyBuffer.length);
      rawNameMap.set("content-length", "Content-Length");
      order.push("content-length");
    }
    if (closeConnection) {
      headers.connection = "close";
      if (!rawNameMap.has("connection")) {
        rawNameMap.set("connection", "Connection");
        order.push("connection");
      }
    } else if (headers.connection === void 0 && request.headers.connection !== void 0) {
      headers.connection = "keep-alive";
      rawNameMap.set("connection", "Connection");
      order.push("connection");
    }
    const serializedChunks = [];
    for (const informational of response.informational ?? []) {
      const infoHeaders = finalizeRawHeaderPairs(
        serializeRawHeaderPairs(informational.rawHeaders, informational.headers).headers,
        serializeRawHeaderPairs(informational.rawHeaders, informational.headers).rawNameMap,
        serializeRawHeaderPairs(informational.rawHeaders, informational.headers).order
      );
      const headerLines2 = infoHeaders.map(([name, value]) => `${name}: ${value}\r
`).join("");
      serializedChunks.push(
        Buffer.from(
          `HTTP/1.1 ${informational.status} ${informational.statusText || HTTP_STATUS_TEXT[informational.status] || ""}\r
${headerLines2}\r
`,
          "latin1"
        )
      );
    }
    const finalHeaders = finalizeRawHeaderPairs(headers, rawNameMap, order);
    const headerLines = finalHeaders.map(([name, value]) => `${name}: ${value}\r
`).join("");
    serializedChunks.push(
      Buffer.from(`HTTP/1.1 ${statusCode} ${statusText}\r
${headerLines}\r
`, "latin1")
    );
    if (bodyAllowed) {
      if (isChunked) {
        if (bodyBuffer.length > 0) {
          serializedChunks.push(Buffer.from(bodyBuffer.length.toString(16) + "\r\n", "latin1"));
          serializedChunks.push(bodyBuffer);
          serializedChunks.push(Buffer.from("\r\n", "latin1"));
        }
        serializedChunks.push(Buffer.from("0\r\n", "latin1"));
        if (Object.keys(trailerInfo.headers).length > 0) {
          const trailerPairs = finalizeRawHeaderPairs(
            trailerInfo.headers,
            trailerInfo.rawNameMap,
            trailerInfo.order
          );
          for (const [name, value] of trailerPairs) {
            serializedChunks.push(Buffer.from(`${name}: ${value}\r
`, "latin1"));
          }
        }
        serializedChunks.push(Buffer.from("\r\n", "latin1"));
      } else if (bodyBuffer.length > 0) {
        serializedChunks.push(bodyBuffer);
      }
    }
    return {
      payload: serializedChunks.length === 1 ? serializedChunks[0] : Buffer.concat(serializedChunks),
      closeConnection
    };
  }
  var HTTP_STATUS_TEXT = {
    100: "Continue",
    101: "Switching Protocols",
    102: "Processing",
    103: "Early Hints",
    200: "OK",
    201: "Created",
    204: "No Content",
    301: "Moved Permanently",
    302: "Found",
    304: "Not Modified",
    400: "Bad Request",
    401: "Unauthorized",
    403: "Forbidden",
    404: "Not Found",
    500: "Internal Server Error"
  };
  function isLoopbackRequestHost(hostname) {
    const bare = hostname.startsWith("[") && hostname.endsWith("]") ? hostname.slice(1, -1) : hostname;
    return bare === "localhost" || bare === "127.0.0.1" || bare === "::1";
  }
  var ServerIncomingMessage = class {
    headers;
    rawHeaders;
    method;
    url;
    socket;
    connection;
    rawBody;
    destroyed = false;
    errored;
    readable = true;
    httpVersion = "1.1";
    httpVersionMajor = 1;
    httpVersionMinor = 1;
    complete = true;
    aborted = false;
    // Readable stream state stub for frameworks that inspect internal state
    _readableState = { flowing: null, length: 0, ended: false, objectMode: false };
    _listeners = {};
    constructor(request) {
      this.headers = request.headers || {};
      this.rawHeaders = request.rawHeaders || [];
      if (!Array.isArray(this.rawHeaders) || this.rawHeaders.length % 2 !== 0) {
        this.rawHeaders = [];
      }
      this.method = request.method || "GET";
      this.url = request.url || "/";
      const fakeSocket = {
        encrypted: false,
        remoteAddress: "127.0.0.1",
        remotePort: 0,
        writable: true,
        on() {
          return fakeSocket;
        },
        once() {
          return fakeSocket;
        },
        removeListener() {
          return fakeSocket;
        },
        destroy() {
        },
        end() {
        }
      };
      this.socket = fakeSocket;
      this.connection = fakeSocket;
      const rawHost = this.headers.host;
      if (typeof rawHost === "string" && rawHost.includes(",")) {
        this.headers.host = rawHost.split(",")[0].trim();
      }
      if (!this.headers.host) {
        this.headers.host = "127.0.0.1";
      }
      if (this.rawHeaders.length === 0) {
        Object.entries(this.headers).forEach(([key, value]) => {
          if (Array.isArray(value)) {
            value.forEach((entry) => {
              this.rawHeaders.push(key, entry);
            });
            return;
          }
          this.rawHeaders.push(key, value);
        });
      }
      if (request.bodyBase64 && typeof Buffer !== "undefined") {
        this.rawBody = Buffer.from(request.bodyBase64, "base64");
      }
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    once(event, listener) {
      const wrapped = (...args) => {
        this.off(event, wrapped);
        listener.call(this, ...args);
      };
      return this.on(event, wrapped);
    }
    off(event, listener) {
      const listeners = this._listeners[event];
      if (!listeners) return this;
      const index = listeners.indexOf(listener);
      if (index !== -1) listeners.splice(index, 1);
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    emit(event, ...args) {
      const listeners = this._listeners[event];
      if (!listeners || listeners.length === 0) return false;
      listeners.slice().forEach((fn) => fn.call(this, ...args));
      return true;
    }
    // Readable stream stubs for framework compatibility
    unpipe() {
      return this;
    }
    pause() {
      return this;
    }
    resume() {
      return this;
    }
    read() {
      return null;
    }
    pipe(dest) {
      return dest;
    }
    isPaused() {
      return false;
    }
    setEncoding() {
      return this;
    }
    destroy(err) {
      this.destroyed = true;
      this.errored = err;
      if (err) {
        this.emit("error", err);
      }
      this.emit("close");
      return this;
    }
    _abort() {
      if (this.aborted) {
        return;
      }
      this.aborted = true;
      const error = createConnResetError("aborted");
      this.emit("aborted");
      this.emit("error", error);
      this.emit("close");
    }
  };
  var ServerResponseBridge = class {
    statusCode = 200;
    statusMessage = "OK";
    headersSent = false;
    writable = true;
    writableFinished = false;
    outputSize = 0;
    _headers = /* @__PURE__ */ new Map();
    _trailers = /* @__PURE__ */ new Map();
    _chunks = [];
    _chunksBytes = 0;
    _listeners = {};
    _closedPromise;
    _resolveClosed = null;
    _connectionEnded = false;
    _connectionReset = false;
    _rawHeaderNames = /* @__PURE__ */ new Map();
    _rawTrailerNames = /* @__PURE__ */ new Map();
    _informational = [];
    _pendingRawInfoBuffer = "";
    constructor() {
      this._closedPromise = new Promise((resolve) => {
        this._resolveClosed = resolve;
      });
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    once(event, listener) {
      const wrapped = (...args) => {
        this.off(event, wrapped);
        listener.call(this, ...args);
      };
      return this.on(event, wrapped);
    }
    off(event, listener) {
      const listeners = this._listeners[event];
      if (!listeners) return this;
      const index = listeners.indexOf(listener);
      if (index !== -1) listeners.splice(index, 1);
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    emit(event, ...args) {
      const listeners = this._listeners[event];
      if (!listeners || listeners.length === 0) return false;
      listeners.slice().forEach((fn) => fn.call(this, ...args));
      return true;
    }
    _emit(event, ...args) {
      this.emit(event, ...args);
    }
    writeHead(statusCode, headers) {
      if (statusCode >= 100 && statusCode < 200 && statusCode !== 101) {
        const informationalHeaders = /* @__PURE__ */ new Map();
        const informationalRawHeaderNames = /* @__PURE__ */ new Map();
        if (headers) {
          if (isFlatHeaderList(headers)) {
            for (let index = 0; index < headers.length; index += 2) {
              const key = headers[index];
              const value = headers[index + 1];
              if (key === void 0 || value === void 0) {
                continue;
              }
              const actualName = validateHeaderName(key).toLowerCase();
              validateHeaderValue(actualName, value);
              informationalHeaders.set(actualName, String(value));
              if (!informationalRawHeaderNames.has(actualName)) {
                informationalRawHeaderNames.set(actualName, key);
              }
            }
          } else if (Array.isArray(headers)) {
            headers.forEach(([key, value]) => {
              const actualName = validateHeaderName(key).toLowerCase();
              validateHeaderValue(actualName, value);
              informationalHeaders.set(actualName, String(value));
              if (!informationalRawHeaderNames.has(actualName)) {
                informationalRawHeaderNames.set(actualName, key);
              }
            });
          } else {
            Object.entries(headers).forEach(([key, value]) => {
              const actualName = validateHeaderName(key).toLowerCase();
              validateHeaderValue(actualName, value);
              informationalHeaders.set(actualName, String(value));
              if (!informationalRawHeaderNames.has(actualName)) {
                informationalRawHeaderNames.set(actualName, key);
              }
            });
          }
        }
        const normalizedHeaders = Array.from(informationalHeaders.entries()).flatMap(([key, value]) => {
          const serialized = serializeHeaderValue(value);
          return Array.isArray(serialized) ? serialized.map((entry) => [key, entry]) : [[key, serialized]];
        });
        const rawHeaders = Array.from(informationalHeaders.entries()).flatMap(([key, value]) => {
          const rawName = informationalRawHeaderNames.get(key) || key;
          const serialized = serializeHeaderValue(value);
          return Array.isArray(serialized) ? serialized.flatMap((entry) => [rawName, entry]) : [rawName, serialized];
        });
        this._informational.push({
          status: statusCode,
          statusText: HTTP_STATUS_TEXT[statusCode],
          headers: normalizedHeaders,
          rawHeaders
        });
        return this;
      }
      this.statusCode = statusCode;
      if (headers) {
        if (isFlatHeaderList(headers)) {
          for (let index = 0; index < headers.length; index += 2) {
            const key = headers[index];
            const value = headers[index + 1];
            if (key !== void 0 && value !== void 0) {
              this.setHeader(key, value);
            }
          }
        } else if (Array.isArray(headers)) {
          headers.forEach(([key, value]) => this.setHeader(key, value));
        } else {
          Object.entries(headers).forEach(
            ([key, value]) => this.setHeader(key, value)
          );
        }
      }
      this.headersSent = true;
      this.outputSize += 64;
      return this;
    }
    setHeader(name, value) {
      if (this.headersSent) {
        throw createErrorWithCode(
          "Cannot set headers after they are sent to the client",
          "ERR_HTTP_HEADERS_SENT"
        );
      }
      const lower = validateHeaderName(name).toLowerCase();
      validateHeaderValue(lower, value);
      const storedValue = Array.isArray(value) ? Array.from(value) : value;
      this._headers.set(lower, storedValue);
      if (!this._rawHeaderNames.has(lower)) {
        this._rawHeaderNames.set(lower, name);
      }
      return this;
    }
    setHeaders(headers) {
      if (this.headersSent) {
        throw createErrorWithCode(
          "Cannot set headers after they are sent to the client",
          "ERR_HTTP_HEADERS_SENT"
        );
      }
      if (!(headers instanceof Headers) && !(headers instanceof Map)) {
        throw createTypeErrorWithCode(
          `The "headers" argument must be an instance of Headers or Map. Received ${formatReceivedType(headers)}`,
          "ERR_INVALID_ARG_TYPE"
        );
      }
      if (headers instanceof Headers) {
        const pending = /* @__PURE__ */ Object.create(null);
        headers.forEach((value, key) => {
          appendNormalizedHeader(pending, key.toLowerCase(), value);
        });
        Object.entries(pending).forEach(([key, value]) => {
          this.setHeader(key, value);
        });
        return this;
      }
      headers.forEach((value, key) => {
        this.setHeader(key, value);
      });
      return this;
    }
    getHeader(name) {
      if (typeof name !== "string") {
        throw createTypeErrorWithCode(
          `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
          "ERR_INVALID_ARG_TYPE"
        );
      }
      const value = this._headers.get(name.toLowerCase());
      return value === void 0 ? void 0 : cloneStoredHeaderValue(value);
    }
    hasHeader(name) {
      if (typeof name !== "string") {
        throw createTypeErrorWithCode(
          `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
          "ERR_INVALID_ARG_TYPE"
        );
      }
      return this._headers.has(name.toLowerCase());
    }
    removeHeader(name) {
      if (typeof name !== "string") {
        throw createTypeErrorWithCode(
          `The "name" argument must be of type string. Received ${formatReceivedType(name)}`,
          "ERR_INVALID_ARG_TYPE"
        );
      }
      const lower = name.toLowerCase();
      this._headers.delete(lower);
      this._rawHeaderNames.delete(lower);
    }
    write(chunk, encodingOrCallback, callback) {
      if (chunk == null) return true;
      this.headersSent = true;
      const buf = typeof chunk === "string" ? Buffer.from(chunk, typeof encodingOrCallback === "string" ? encodingOrCallback : void 0) : chunk;
      if (this._chunksBytes + buf.byteLength > MAX_HTTP_BODY_BYTES) {
        throw new Error("ERR_HTTP_BODY_TOO_LARGE: response body exceeds " + MAX_HTTP_BODY_BYTES + " byte limit");
      }
      this._chunks.push(buf);
      this._chunksBytes += buf.byteLength;
      this.outputSize += buf.byteLength;
      const writeCallback = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      if (typeof writeCallback === "function") {
        queueMicrotask(writeCallback);
      }
      return true;
    }
    end(chunkOrCallback, encodingOrCallback, callback) {
      let chunk;
      let endCallback;
      if (typeof chunkOrCallback === "function") {
        endCallback = chunkOrCallback;
      } else {
        chunk = chunkOrCallback;
        endCallback = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      }
      if (chunk != null) {
        if (typeof chunk === "string" && typeof encodingOrCallback === "string") {
          this.write(Buffer.from(chunk, encodingOrCallback));
        } else {
          this.write(chunk);
        }
      }
      this._finalize();
      if (typeof endCallback === "function") {
        queueMicrotask(endCallback);
      }
      return this;
    }
    getHeaderNames() {
      return Array.from(this._headers.keys());
    }
    getRawHeaderNames() {
      return Array.from(this._headers.keys()).map((key) => this._rawHeaderNames.get(key) || key);
    }
    getHeaders() {
      const result = /* @__PURE__ */ Object.create(null);
      for (const [key, value] of this._headers) {
        result[key] = cloneStoredHeaderValue(value);
      }
      return result;
    }
    // Writable stream state stub for frameworks that inspect internal state
    _writableState = { length: 0, ended: false, finished: false, objectMode: false, corked: 0 };
    // Fake socket for frameworks that access res.socket/res.connection
    socket = {
      writable: true,
      writableCorked: 0,
      writableHighWaterMark: 16 * 1024,
      on: () => this.socket,
      once: () => this.socket,
      removeListener: () => this.socket,
      destroy: () => {
        this._connectionReset = true;
        this._finalize();
      },
      end: () => {
        this._connectionEnded = true;
      },
      cork: () => {
        this._writableState.corked += 1;
        this.socket.writableCorked = this._writableState.corked;
      },
      uncork: () => {
        this._writableState.corked = Math.max(0, this._writableState.corked - 1);
        this.socket.writableCorked = this._writableState.corked;
      },
      write: (_chunk, callback) => {
        if (typeof callback === "function") {
          queueMicrotask(callback);
        }
        return true;
      }
    };
    connection = this.socket;
    // Node.js http.ServerResponse socket/stream compatibility stubs
    assignSocket() {
    }
    detachSocket() {
    }
    writeContinue() {
      this.writeHead(100);
    }
    writeProcessing() {
      this.writeHead(102);
    }
    addTrailers(headers) {
      if (Array.isArray(headers)) {
        for (let index = 0; index < headers.length; index += 2) {
          const key = headers[index];
          const value = headers[index + 1];
          if (key === void 0 || value === void 0) {
            continue;
          }
          const actualName = validateHeaderName(key).toLowerCase();
          validateHeaderValue(actualName, value);
          this._trailers.set(actualName, String(value));
          if (!this._rawTrailerNames.has(actualName)) {
            this._rawTrailerNames.set(actualName, key);
          }
        }
        return;
      }
      Object.entries(headers).forEach(([key, value]) => {
        const actualName = validateHeaderName(key).toLowerCase();
        validateHeaderValue(actualName, value);
        this._trailers.set(actualName, String(value));
        if (!this._rawTrailerNames.has(actualName)) {
          this._rawTrailerNames.set(actualName, key);
        }
      });
    }
    cork() {
      this.socket.cork();
    }
    uncork() {
      this.socket.uncork();
    }
    setTimeout(_msecs) {
      return this;
    }
    get writableCorked() {
      return Number(this.socket.writableCorked || 0);
    }
    flushHeaders() {
      this.headersSent = true;
    }
    destroy(err) {
      this._connectionReset = true;
      if (err) {
        this._emit("error", err);
      }
      this._finalize();
    }
    async waitForClose() {
      await this._closedPromise;
    }
    serialize() {
      const bodyBuffer = this._chunks.length > 0 ? Buffer.concat(this._chunks) : Buffer.alloc(0);
      const serializedHeaders = Array.from(this._headers.entries()).flatMap(([key, value]) => {
        const serialized = serializeHeaderValue(value);
        if (Array.isArray(serialized)) {
          if (key === "set-cookie") {
            return serialized.map((entry) => [key, entry]);
          }
          return [[key, serialized.join(", ")]];
        }
        return [[key, serialized]];
      });
      const rawHeaders = Array.from(this._headers.entries()).flatMap(([key, value]) => {
        const rawName = this._rawHeaderNames.get(key) || key;
        const serialized = serializeHeaderValue(value);
        if (Array.isArray(serialized)) {
          if (key === "set-cookie") {
            return serialized.flatMap((entry) => [rawName, entry]);
          }
          return [rawName, serialized.join(", ")];
        }
        return [rawName, serialized];
      });
      const serializedTrailers = Array.from(this._trailers.entries()).flatMap(([key, value]) => {
        const serialized = serializeHeaderValue(value);
        return Array.isArray(serialized) ? serialized.map((entry) => [key, entry]) : [[key, serialized]];
      });
      const rawTrailers = Array.from(this._trailers.entries()).flatMap(([key, value]) => {
        const rawName = this._rawTrailerNames.get(key) || key;
        const serialized = serializeHeaderValue(value);
        return Array.isArray(serialized) ? serialized.flatMap((entry) => [rawName, entry]) : [rawName, serialized];
      });
      return {
        status: this.statusCode,
        headers: serializedHeaders,
        rawHeaders,
        informational: this._informational.length > 0 ? [...this._informational] : void 0,
        body: bodyBuffer.toString("base64"),
        bodyEncoding: "base64",
        trailers: serializedTrailers.length > 0 ? serializedTrailers : void 0,
        rawTrailers: rawTrailers.length > 0 ? rawTrailers : void 0,
        connectionEnded: this._connectionEnded,
        connectionReset: this._connectionReset
      };
    }
    _writeRaw(chunk, callback) {
      this._pendingRawInfoBuffer += String(chunk);
      this._flushPendingRawInformational();
      if (typeof callback === "function") {
        queueMicrotask(callback);
      }
      return true;
    }
    _finalize() {
      if (this.writableFinished) {
        return;
      }
      this.writableFinished = true;
      this.writable = false;
      this._writableState.ended = true;
      this._writableState.finished = true;
      this._emit("finish");
      this._emit("close");
      this._resolveClosed?.();
      this._resolveClosed = null;
    }
    _flushPendingRawInformational() {
      let separatorIndex = this._pendingRawInfoBuffer.indexOf("\r\n\r\n");
      while (separatorIndex !== -1) {
        const rawFrame = this._pendingRawInfoBuffer.slice(0, separatorIndex);
        this._pendingRawInfoBuffer = this._pendingRawInfoBuffer.slice(separatorIndex + 4);
        const [statusLine, ...headerLines] = rawFrame.split("\r\n");
        const statusMatch = /^HTTP\/1\.[01]\s+(\d{3})(?:\s+(.*))?$/.exec(statusLine);
        if (!statusMatch) {
          separatorIndex = this._pendingRawInfoBuffer.indexOf("\r\n\r\n");
          continue;
        }
        const status = Number(statusMatch[1]);
        if (status >= 100 && status < 200 && status !== 101) {
          const headers = [];
          const rawHeaders = [];
          for (const headerLine of headerLines) {
            const separator = headerLine.indexOf(":");
            if (separator === -1) {
              continue;
            }
            const key = headerLine.slice(0, separator).trim();
            const value = headerLine.slice(separator + 1).trim();
            headers.push([key.toLowerCase(), value]);
            rawHeaders.push(key, value);
          }
          this._informational.push({
            status,
            statusText: statusMatch[2] || HTTP_STATUS_TEXT[status] || void 0,
            headers,
            rawHeaders
          });
        }
        separatorIndex = this._pendingRawInfoBuffer.indexOf("\r\n\r\n");
      }
    }
  };
  var Server = class {
    listening = false;
    _listeners = {};
    _serverId;
    _listenPromise = null;
    _address = null;
    _handleId = null;
    _hostCloseWaitStarted = false;
    _activeRequestDispatches = 0;
    _closePending = false;
    _closeRunning = false;
    _closeCallbacks = [];
    /** @internal Request listener stored on the instance (replaces serverRequestListeners Map). */
    _requestListener;
    constructor(requestListener) {
      this._serverId = nextServerId++;
      this._requestListener = requestListener ?? (() => void 0);
      serverInstances.set(this._serverId, this);
    }
    /** @internal Bridge-visible server ID for loopback self-dispatch. */
    get _bridgeServerId() {
      return this._serverId;
    }
    /** @internal Emit an event — used by upgrade dispatch to fire 'upgrade' events. */
    _emit(event, ...args) {
      const listeners = this._listeners[event];
      if (!listeners || listeners.length === 0) return;
      listeners.slice().forEach((listener) => listener.call(this, ...args));
    }
    _finishStart(resultJson) {
      const result = JSON.parse(resultJson);
      this._address = result.address;
      this.listening = true;
      this._handleId = `http-server:${this._serverId}`;
      debugBridgeNetwork("server listening", this._serverId, this._address);
      if (typeof _registerHandle === "function") {
        _registerHandle(this._handleId, "http server");
      }
      this._startHostCloseWait();
    }
    _completeClose() {
      this.listening = false;
      this._address = null;
      serverInstances.delete(this._serverId);
      if (this._handleId && typeof _unregisterHandle === "function") {
        _unregisterHandle(this._handleId);
      }
      this._handleId = null;
    }
    _beginRequestDispatch() {
      this._activeRequestDispatches += 1;
    }
    _endRequestDispatch() {
      this._activeRequestDispatches = Math.max(0, this._activeRequestDispatches - 1);
      if (this._closePending && this._activeRequestDispatches === 0) {
        this._closePending = false;
        queueMicrotask(() => {
          this._startClose();
        });
      }
    }
    _startHostCloseWait() {
      this._hostCloseWaitStarted = true;
    }
    async _start(port, hostname) {
      if (typeof _networkHttpServerListenRaw === "undefined") {
        throw new Error(
          "http.createServer requires kernel-backed network bridge support"
        );
      }
      debugBridgeNetwork("server listen start", this._serverId, port, hostname);
      const resultJson = await _networkHttpServerListenRaw.apply(
        void 0,
        [JSON.stringify({ serverId: this._serverId, port, hostname })],
        { result: { promise: true } }
      );
      this._finishStart(resultJson);
    }
    listen(portOrCb, hostOrCb, cb) {
      const port = typeof portOrCb === "number" ? portOrCb : void 0;
      const hostname = typeof hostOrCb === "string" ? hostOrCb : void 0;
      const callback = typeof cb === "function" ? cb : typeof hostOrCb === "function" ? hostOrCb : typeof portOrCb === "function" ? portOrCb : void 0;
      if (!this._listenPromise) {
        this._listenPromise = this._start(port, hostname).then(() => {
          this._emit("listening");
          callback?.call(this);
        }).catch((error) => {
          this._emit("error", error);
        });
      }
      return this;
    }
    close(cb) {
      debugBridgeNetwork("server close requested", this._serverId, this.listening);
      if (cb) {
        this._closeCallbacks.push(cb);
      }
      if (this._activeRequestDispatches > 0) {
        this._closePending = true;
        return this;
      }
      queueMicrotask(() => {
        this._startClose();
      });
      return this;
    }
    _startClose() {
      if (this._closeRunning) {
        return;
      }
      this._closeRunning = true;
      const run = async () => {
        try {
          if (this._listenPromise) {
            await this._listenPromise;
          }
          if (this.listening && typeof _networkHttpServerCloseRaw !== "undefined") {
            debugBridgeNetwork("server close bridge call", this._serverId);
            await _networkHttpServerCloseRaw.apply(void 0, [this._serverId], {
              result: { promise: true }
            });
          }
          this._completeClose();
          debugBridgeNetwork("server close complete", this._serverId);
          const callbacks = this._closeCallbacks.splice(0);
          callbacks.forEach((callback) => callback());
          this._emit("close");
        } catch (err) {
          const error = err instanceof Error ? err : new Error(String(err));
          debugBridgeNetwork("server close error", this._serverId, error.message);
          const callbacks = this._closeCallbacks.splice(0);
          callbacks.forEach((callback) => callback(error));
          this._emit("error", error);
        } finally {
          this._closeRunning = false;
        }
      };
      void run();
    }
    address() {
      return this._address;
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    once(event, listener) {
      const wrapped = (...args) => {
        this.off(event, wrapped);
        listener.call(this, ...args);
      };
      return this.on(event, wrapped);
    }
    off(event, listener) {
      const listeners = this._listeners[event];
      if (!listeners) return this;
      const index = listeners.indexOf(listener);
      if (index !== -1) listeners.splice(index, 1);
      return this;
    }
    removeListener(event, listener) {
      return this.off(event, listener);
    }
    removeAllListeners(event) {
      if (event) {
        delete this._listeners[event];
      } else {
        this._listeners = {};
      }
      return this;
    }
    listenerCount(event) {
      return this._listeners[event]?.length || 0;
    }
    // Node.js Server timeout properties (no-op in sandbox)
    keepAliveTimeout = 5e3;
    requestTimeout = 3e5;
    headersTimeout = 6e4;
    timeout = 0;
    maxRequestsPerSocket = 0;
    setTimeout(_msecs, _callback) {
      if (typeof _msecs === "number") this.timeout = _msecs;
      return this;
    }
    ref() {
      return this;
    }
    unref() {
      return this;
    }
  };
  function ServerCallable(requestListener) {
    return new Server(requestListener);
  }
  ServerCallable.prototype = Server.prototype;
  async function dispatchServerRequest(serverId, requestJson) {
    const server = serverInstances.get(serverId);
    if (!server) {
      throw new Error(`Unknown HTTP server: ${serverId}`);
    }
    const listener = server._requestListener;
    server._beginRequestDispatch();
    const request = JSON.parse(requestJson);
    const incoming = new ServerIncomingMessage(request);
    const outgoing = new ServerResponseBridge();
    incoming.socket = outgoing.socket;
    incoming.connection = outgoing.socket;
    const pendingImmediates = [];
    const pendingTimers = [];
    const trackedTimers = /* @__PURE__ */ new Map();
    let consumedTimerCount = 0;
    let consumedImmediateCount = 0;
    try {
      try {
        const originalSetImmediate = globalThis.setImmediate;
        const originalSetTimeout = globalThis.setTimeout;
        const originalClearTimeout = globalThis.clearTimeout;
        if (typeof originalSetImmediate === "function") {
          globalThis.setImmediate = ((callback, ...args) => {
            const pending = new Promise((resolve) => {
              queueMicrotask(() => {
                try {
                  callback(...args);
                } finally {
                  resolve();
                }
              });
            });
            pendingImmediates.push(pending);
            return 0;
          });
        }
        if (typeof originalSetTimeout === "function") {
          globalThis.setTimeout = ((callback, delay, ...args) => {
            if (typeof callback !== "function") {
              return originalSetTimeout(callback, delay, ...args);
            }
            const normalizedDelay = typeof delay === "number" && Number.isFinite(delay) ? Math.max(0, delay) : 0;
            if (normalizedDelay > 1e3) {
              return originalSetTimeout(callback, normalizedDelay, ...args);
            }
            let resolvePending;
            const pending = new Promise((resolve) => {
              resolvePending = resolve;
            });
            let handle;
            handle = originalSetTimeout(() => {
              trackedTimers.delete(handle);
              try {
                callback(...args);
              } finally {
                resolvePending();
              }
            }, normalizedDelay);
            trackedTimers.set(handle, resolvePending);
            pendingTimers.push(pending);
            return handle;
          });
        }
        if (typeof originalClearTimeout === "function") {
          globalThis.clearTimeout = ((handle) => {
            if (handle != null) {
              const resolvePending = trackedTimers.get(handle);
              if (resolvePending) {
                trackedTimers.delete(handle);
                resolvePending();
              }
            }
            return originalClearTimeout(handle);
          });
        }
        try {
          const listenerResult = listener(incoming, outgoing);
          if (incoming.rawBody && incoming.rawBody.length > 0) {
            incoming.emit("data", incoming.rawBody);
          }
          incoming.emit("end");
          await Promise.resolve(listenerResult);
          while (consumedTimerCount < pendingTimers.length || consumedImmediateCount < pendingImmediates.length) {
            const pending = [
              ...pendingTimers.slice(consumedTimerCount),
              ...pendingImmediates.slice(consumedImmediateCount)
            ];
            consumedTimerCount = pendingTimers.length;
            consumedImmediateCount = pendingImmediates.length;
            await Promise.allSettled(pending);
          }
        } finally {
          if (typeof originalSetImmediate === "function") {
            globalThis.setImmediate = originalSetImmediate;
          }
          if (typeof originalSetTimeout === "function") {
            globalThis.setTimeout = originalSetTimeout;
          }
          if (typeof originalClearTimeout === "function") {
            globalThis.clearTimeout = originalClearTimeout;
          }
        }
      } catch (err) {
        outgoing.statusCode = 500;
        try {
          outgoing.end(err instanceof Error ? `Error: ${err.message}` : "Error");
        } catch {
          if (!outgoing.writableFinished) outgoing.end();
        }
      }
      if (!outgoing.writableFinished) {
        outgoing.end();
      }
      await outgoing.waitForClose();
      await Promise.allSettled([...pendingTimers, ...pendingImmediates]);
      return JSON.stringify(outgoing.serialize());
    } finally {
      server._endRequestDispatch();
    }
  }
  async function dispatchHttp2CompatibilityRequest(serverId, requestId) {
    const pending = pendingHttp2CompatRequests.get(requestId);
    if (!pending || pending.serverId !== serverId || typeof _networkHttp2ServerRespondRaw === "undefined") {
      return;
    }
    pendingHttp2CompatRequests.delete(requestId);
    const server = http2Servers.get(serverId);
    if (!server) {
      _networkHttp2ServerRespondRaw.applySync(void 0, [
        serverId,
        requestId,
        JSON.stringify({
          status: 500,
          headers: [["content-type", "text/plain"]],
          body: "Unknown HTTP/2 server",
          bodyEncoding: "utf8"
        })
      ]);
      return;
    }
    const request = JSON.parse(pending.requestJson);
    const incoming = new ServerIncomingMessage(request);
    const outgoing = new ServerResponseBridge();
    incoming.socket = outgoing.socket;
    incoming.connection = outgoing.socket;
    try {
      server.emit("request", incoming, outgoing);
      if (incoming.rawBody && incoming.rawBody.length > 0) {
        incoming.emit("data", incoming.rawBody);
      }
      incoming.emit("end");
      if (!outgoing.writableFinished) {
        outgoing.end();
      }
      await outgoing.waitForClose();
      _networkHttp2ServerRespondRaw.applySync(void 0, [
        serverId,
        requestId,
        JSON.stringify(outgoing.serialize())
      ]);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      _networkHttp2ServerRespondRaw.applySync(void 0, [
        serverId,
        requestId,
        JSON.stringify({
          status: 500,
          headers: [["content-type", "text/plain"]],
          body: `Error: ${message}`,
          bodyEncoding: "utf8"
        })
      ]);
    }
  }
  async function dispatchLoopbackServerRequest(serverOrId, requestInput) {
    const server = typeof serverOrId === "number" ? serverInstances.get(serverOrId) : serverOrId;
    if (!server) {
      throw new Error(
        `Unknown HTTP server: ${typeof serverOrId === "number" ? serverOrId : "<detached>"}`
      );
    }
    const request = typeof requestInput === "string" ? JSON.parse(requestInput) : requestInput;
    const incoming = new ServerIncomingMessage(request);
    const outgoing = new ServerResponseBridge();
    incoming.socket = outgoing.socket;
    incoming.connection = outgoing.socket;
    const pendingImmediates = [];
    const pendingTimers = [];
    const trackedTimers = /* @__PURE__ */ new Map();
    let consumedTimerCount = 0;
    let consumedImmediateCount = 0;
    server._beginRequestDispatch();
    try {
      try {
        const originalSetImmediate = globalThis.setImmediate;
        const originalSetTimeout = globalThis.setTimeout;
        const originalClearTimeout = globalThis.clearTimeout;
        if (typeof originalSetImmediate === "function") {
          globalThis.setImmediate = ((callback, ...args) => {
            const pending = new Promise((resolve) => {
              queueMicrotask(() => {
                try {
                  callback(...args);
                } finally {
                  resolve();
                }
              });
            });
            pendingImmediates.push(pending);
            return 0;
          });
        }
        if (typeof originalSetTimeout === "function") {
          globalThis.setTimeout = ((callback, delay, ...args) => {
            if (typeof callback !== "function") {
              return originalSetTimeout(callback, delay, ...args);
            }
            const normalizedDelay = typeof delay === "number" && Number.isFinite(delay) ? Math.max(0, delay) : 0;
            if (normalizedDelay > 1e3) {
              return originalSetTimeout(callback, normalizedDelay, ...args);
            }
            let resolvePending;
            const pending = new Promise((resolve) => {
              resolvePending = resolve;
            });
            let handle;
            handle = originalSetTimeout(() => {
              trackedTimers.delete(handle);
              try {
                callback(...args);
              } finally {
                resolvePending();
              }
            }, normalizedDelay);
            trackedTimers.set(handle, resolvePending);
            pendingTimers.push(pending);
            return handle;
          });
        }
        if (typeof originalClearTimeout === "function") {
          globalThis.clearTimeout = ((handle) => {
            if (handle != null) {
              const resolvePending = trackedTimers.get(handle);
              if (resolvePending) {
                trackedTimers.delete(handle);
                resolvePending();
              }
            }
            return originalClearTimeout(handle);
          });
        }
        try {
          const listenerResult = server._requestListener(incoming, outgoing);
          if (incoming.rawBody && incoming.rawBody.length > 0) {
            incoming.emit("data", incoming.rawBody);
          }
          incoming.emit("end");
          await Promise.resolve(listenerResult);
          while (consumedTimerCount < pendingTimers.length || consumedImmediateCount < pendingImmediates.length) {
            const pending = [
              ...pendingTimers.slice(consumedTimerCount),
              ...pendingImmediates.slice(consumedImmediateCount)
            ];
            consumedTimerCount = pendingTimers.length;
            consumedImmediateCount = pendingImmediates.length;
            await Promise.allSettled(pending);
          }
        } finally {
          if (typeof originalSetImmediate === "function") {
            globalThis.setImmediate = originalSetImmediate;
          }
          if (typeof originalSetTimeout === "function") {
            globalThis.setTimeout = originalSetTimeout;
          }
          if (typeof originalClearTimeout === "function") {
            globalThis.clearTimeout = originalClearTimeout;
          }
        }
      } catch (err) {
        outgoing.statusCode = 500;
        try {
          outgoing.end(err instanceof Error ? `Error: ${err.message}` : "Error");
        } catch {
          if (!outgoing.writableFinished) outgoing.end();
        }
      }
      if (!outgoing.writableFinished) {
        outgoing.end();
      }
      await outgoing.waitForClose();
      await Promise.allSettled([...pendingTimers, ...pendingImmediates]);
      let aborted = false;
      return {
        responseJson: JSON.stringify(outgoing.serialize()),
        abortRequest: () => {
          if (aborted) {
            return;
          }
          aborted = true;
          incoming._abort();
        }
      };
    } finally {
      server._endRequestDispatch();
    }
  }
  function dispatchSocketRequest(event, serverId, requestJson, headBase64, socketId) {
    const server = serverInstances.get(serverId);
    if (!server) {
      throw new Error(`Unknown HTTP server for ${event}: ${serverId}`);
    }
    const request = JSON.parse(requestJson);
    const incoming = new ServerIncomingMessage(request);
    const head = typeof Buffer !== "undefined" ? Buffer.from(headBase64, "base64") : new Uint8Array(0);
    const hostHeader = incoming.headers["host"];
    const socket = new UpgradeSocket(socketId, {
      host: (Array.isArray(hostHeader) ? hostHeader[0] : hostHeader)?.split(":")[0] || "127.0.0.1"
    });
    upgradeSocketInstances.set(socketId, socket);
    server._emit(event, incoming, socket, head);
  }
  var upgradeSocketInstances = /* @__PURE__ */ new Map();
  var UpgradeSocket = class {
    remoteAddress;
    remotePort;
    localAddress = "127.0.0.1";
    localPort = 0;
    connecting = false;
    destroyed = false;
    writable = true;
    readable = true;
    readyState = "open";
    bytesWritten = 0;
    _listeners = {};
    _socketId;
    // Readable stream state stub for ws compatibility (socketOnClose checks _readableState.endEmitted)
    _readableState = { endEmitted: false };
    _writableState = { finished: false, errorEmitted: false };
    constructor(socketId, options) {
      this._socketId = socketId;
      this.remoteAddress = options?.host || "127.0.0.1";
      this.remotePort = options?.port || 80;
    }
    setTimeout(_ms, _cb) {
      return this;
    }
    setNoDelay(_noDelay) {
      return this;
    }
    setKeepAlive(_enable, _delay) {
      return this;
    }
    ref() {
      return this;
    }
    unref() {
      return this;
    }
    cork() {
    }
    uncork() {
    }
    pause() {
      return this;
    }
    resume() {
      return this;
    }
    address() {
      return { address: this.localAddress, family: "IPv4", port: this.localPort };
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    addListener(event, listener) {
      return this.on(event, listener);
    }
    once(event, listener) {
      const wrapper = (...args) => {
        this.off(event, wrapper);
        listener(...args);
      };
      return this.on(event, wrapper);
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
    removeAllListeners(event) {
      if (event) {
        delete this._listeners[event];
      } else {
        this._listeners = {};
      }
      return this;
    }
    emit(event, ...args) {
      const handlers = this._listeners[event];
      if (handlers) handlers.slice().forEach((fn) => fn.call(this, ...args));
      return handlers !== void 0 && handlers.length > 0;
    }
    listenerCount(event) {
      return this._listeners[event]?.length || 0;
    }
    write(data, encodingOrCb, cb) {
      if (this.destroyed) return false;
      const callback = typeof encodingOrCb === "function" ? encodingOrCb : cb;
      if (typeof _upgradeSocketWriteRaw !== "undefined") {
        let base64;
        if (typeof Buffer !== "undefined" && Buffer.isBuffer(data)) {
          base64 = data.toString("base64");
        } else if (typeof data === "string") {
          base64 = typeof Buffer !== "undefined" ? Buffer.from(data).toString("base64") : btoa(data);
        } else if (data instanceof Uint8Array) {
          base64 = typeof Buffer !== "undefined" ? Buffer.from(data).toString("base64") : btoa(String.fromCharCode(...data));
        } else {
          base64 = typeof Buffer !== "undefined" ? Buffer.from(String(data)).toString("base64") : btoa(String(data));
        }
        this.bytesWritten += base64.length;
        _upgradeSocketWriteRaw.applySync(void 0, [this._socketId, base64]);
      }
      if (callback) callback();
      return true;
    }
    end(data) {
      if (data) this.write(data);
      if (typeof _upgradeSocketEndRaw !== "undefined" && !this.destroyed) {
        _upgradeSocketEndRaw.applySync(void 0, [this._socketId]);
      }
      this.writable = false;
      this.emit("finish");
      return this;
    }
    destroy(err) {
      if (this.destroyed) return this;
      this.destroyed = true;
      this.writable = false;
      this.readable = false;
      this._readableState.endEmitted = true;
      this._writableState.finished = true;
      if (typeof _upgradeSocketDestroyRaw !== "undefined") {
        _upgradeSocketDestroyRaw.applySync(void 0, [this._socketId]);
      }
      upgradeSocketInstances.delete(this._socketId);
      if (err) this.emit("error", err);
      this.emit("close", false);
      return this;
    }
    // Push data received from the host into this socket
    _pushData(data) {
      this.emit("data", data);
    }
    // Signal end-of-stream from the host
    _pushEnd() {
      this.readable = false;
      this._readableState.endEmitted = true;
      this._writableState.finished = true;
      this.emit("end");
      this.emit("close", false);
      upgradeSocketInstances.delete(this._socketId);
    }
  };
  function dispatchUpgradeRequest(serverId, requestJson, headBase64, socketId) {
    dispatchSocketRequest("upgrade", serverId, requestJson, headBase64, socketId);
  }
  function dispatchConnectRequest(serverId, requestJson, headBase64, socketId) {
    dispatchSocketRequest("connect", serverId, requestJson, headBase64, socketId);
  }
  function onUpgradeSocketData(socketId, dataBase64) {
    const socket = upgradeSocketInstances.get(socketId);
    if (socket) {
      const data = typeof Buffer !== "undefined" ? Buffer.from(dataBase64, "base64") : new Uint8Array(0);
      socket._pushData(data);
    }
  }
  function onUpgradeSocketEnd(socketId) {
    const socket = upgradeSocketInstances.get(socketId);
    if (socket) {
      socket._pushEnd();
    }
  }
  function ServerResponseCallable() {
    this.statusCode = 200;
    this.statusMessage = "OK";
    this.headersSent = false;
    this.writable = true;
    this.writableFinished = false;
    this.outputSize = 0;
    this._headers = /* @__PURE__ */ new Map();
    this._trailers = /* @__PURE__ */ new Map();
    this._rawHeaderNames = /* @__PURE__ */ new Map();
    this._rawTrailerNames = /* @__PURE__ */ new Map();
    this._informational = [];
    this._pendingRawInfoBuffer = "";
    this._chunks = [];
    this._chunksBytes = 0;
    this._listeners = {};
    this._closedPromise = new Promise((resolve) => {
      this._resolveClosed = resolve;
    });
    this._connectionEnded = false;
    this._connectionReset = false;
    this._writableState = { length: 0, ended: false, finished: false, objectMode: false, corked: 0 };
    const fakeSocket = {
      writable: true,
      writableCorked: 0,
      writableHighWaterMark: 16 * 1024,
      on() {
        return fakeSocket;
      },
      once() {
        return fakeSocket;
      },
      removeListener() {
        return fakeSocket;
      },
      destroy() {
      },
      end() {
      },
      cork() {
      },
      uncork() {
      },
      write() {
        return true;
      }
    };
    this.socket = fakeSocket;
    this.connection = fakeSocket;
  }
  ServerResponseCallable.prototype = Object.create(ServerResponseBridge.prototype, {
    constructor: { value: ServerResponseCallable, writable: true, configurable: true }
  });
  function createHttpModule(protocol) {
    const defaultProtocol = protocol === "https" ? "https:" : "http:";
    const moduleAgent = new Agent({ keepAlive: false });
    _moduleGlobalAgent = moduleAgent;
    function ensureProtocol(opts) {
      if (!opts.protocol) return { ...opts, protocol: defaultProtocol };
      return opts;
    }
    return {
      request(options, optionsOrCallback, maybeCallback) {
        let opts;
        const callback = typeof optionsOrCallback === "function" ? optionsOrCallback : maybeCallback;
        if (typeof options === "string") {
          const url = new URL(options);
          opts = {
            protocol: url.protocol,
            hostname: url.hostname,
            port: url.port,
            path: url.pathname + url.search,
            ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
          };
        } else if (options instanceof URL) {
          opts = {
            protocol: options.protocol,
            hostname: options.hostname,
            port: options.port,
            path: options.pathname + options.search,
            ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
          };
        } else {
          opts = {
            ...options,
            ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
          };
        }
        return new ClientRequest(ensureProtocol(opts), callback);
      },
      get(options, optionsOrCallback, maybeCallback) {
        let opts;
        const callback = typeof optionsOrCallback === "function" ? optionsOrCallback : maybeCallback;
        if (typeof options === "string") {
          const url = new URL(options);
          opts = {
            protocol: url.protocol,
            hostname: url.hostname,
            port: url.port,
            path: url.pathname + url.search,
            method: "GET",
            ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
          };
        } else if (options instanceof URL) {
          opts = {
            protocol: options.protocol,
            hostname: options.hostname,
            port: options.port,
            path: options.pathname + options.search,
            method: "GET",
            ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {}
          };
        } else {
          opts = {
            ...options,
            ...typeof optionsOrCallback === "object" && optionsOrCallback ? optionsOrCallback : {},
            method: "GET"
          };
        }
        const req = new ClientRequest(ensureProtocol(opts), callback);
        req.end();
        return req;
      },
      createServer(_optionsOrListener, maybeListener) {
        const listener = typeof _optionsOrListener === "function" ? _optionsOrListener : maybeListener;
        return new Server(listener);
      },
      Agent,
      globalAgent: moduleAgent,
      Server: ServerCallable,
      ServerResponse: ServerResponseCallable,
      IncomingMessage,
      ClientRequest,
      validateHeaderName,
      validateHeaderValue,
      _checkIsHttpToken: checkIsHttpToken,
      _checkInvalidHeaderChar: checkInvalidHeaderChar,
      maxHeaderSize: 65535,
      METHODS: [...HTTP_METHODS],
      STATUS_CODES: HTTP_STATUS_TEXT
    };
  }
  var http = createHttpModule("http");
  var https = createHttpModule("https");
  var HTTP2_K_SOCKET = /* @__PURE__ */ Symbol.for("secure-exec.http2.kSocket");
  var HTTP2_OPTIONS = /* @__PURE__ */ Symbol("options");
  var http2Servers = /* @__PURE__ */ new Map();
  var http2Sessions = /* @__PURE__ */ new Map();
  var http2Streams = /* @__PURE__ */ new Map();
  var pendingHttp2ClientStreamEvents = /* @__PURE__ */ new Map();
  var scheduledHttp2ClientStreamFlushes = /* @__PURE__ */ new Set();
  var queuedHttp2DispatchEvents = [];
  var pendingHttp2CompatRequests = /* @__PURE__ */ new Map();
  var scheduledHttp2DispatchDrain = false;
  var nextHttp2ServerId = 1;
  var Http2EventEmitter = class {
    _listeners = {};
    _onceListeners = {};
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    addListener(event, listener) {
      return this.on(event, listener);
    }
    once(event, listener) {
      if (!this._onceListeners[event]) this._onceListeners[event] = [];
      this._onceListeners[event].push(listener);
      return this;
    }
    removeListener(event, listener) {
      const remove = (target) => {
        if (!target) return;
        const index = target.indexOf(listener);
        if (index !== -1) target.splice(index, 1);
      };
      remove(this._listeners[event]);
      remove(this._onceListeners[event]);
      return this;
    }
    off(event, listener) {
      return this.removeListener(event, listener);
    }
    listenerCount(event) {
      return (this._listeners[event]?.length ?? 0) + (this._onceListeners[event]?.length ?? 0);
    }
    setMaxListeners(_value) {
      return this;
    }
    emit(event, ...args) {
      let handled = false;
      const listeners = this._listeners[event];
      if (listeners) {
        for (const listener of [...listeners]) {
          listener.call(this, ...args);
          handled = true;
        }
      }
      const onceListeners = this._onceListeners[event];
      if (onceListeners) {
        this._onceListeners[event] = [];
        for (const listener of [...onceListeners]) {
          listener.call(this, ...args);
          handled = true;
        }
      }
      return handled;
    }
  };
  var Http2SocketProxy = class extends Http2EventEmitter {
    allowHalfOpen = false;
    encrypted = false;
    localAddress = "127.0.0.1";
    localPort = 0;
    localFamily = "IPv4";
    remoteAddress = "127.0.0.1";
    remotePort = 0;
    remoteFamily = "IPv4";
    servername;
    alpnProtocol = false;
    readable = true;
    writable = true;
    destroyed = false;
    _bridgeReadPollTimer = null;
    _loopbackServer = null;
    _onDestroy;
    _destroyCallbackInvoked = false;
    constructor(state, onDestroy) {
      super();
      this._onDestroy = onDestroy;
      this._applyState(state);
    }
    _applyState(state) {
      if (!state) return;
      this.allowHalfOpen = state.allowHalfOpen === true;
      this.encrypted = state.encrypted === true;
      this.localAddress = state.localAddress ?? this.localAddress;
      this.localPort = state.localPort ?? this.localPort;
      this.localFamily = state.localFamily ?? this.localFamily;
      this.remoteAddress = state.remoteAddress ?? this.remoteAddress;
      this.remotePort = state.remotePort ?? this.remotePort;
      this.remoteFamily = state.remoteFamily ?? this.remoteFamily;
      this.servername = state.servername;
      this.alpnProtocol = state.alpnProtocol ?? this.alpnProtocol;
    }
    _clearTimeoutTimer() {
    }
    _emitNet(event, error) {
      if (event === "error" && error) {
        this.emit("error", error);
        return;
      }
      if (event === "close") {
        if (!this._destroyCallbackInvoked) {
          this._destroyCallbackInvoked = true;
          queueMicrotask(() => {
            this._onDestroy?.();
          });
        }
        this.emit("close");
      }
    }
    end() {
      this.destroyed = true;
      this.readable = false;
      this.writable = false;
      this.emit("close");
      return this;
    }
    destroy() {
      if (this.destroyed) {
        return this;
      }
      this.destroyed = true;
      this.readable = false;
      this.writable = false;
      this._emitNet("close");
      return this;
    }
  };
  function createHttp2ArgTypeError(argumentName, expected, value) {
    return createTypeErrorWithCode(
      `The "${argumentName}" argument must be of type ${expected}. Received ${formatReceivedType(value)}`,
      "ERR_INVALID_ARG_TYPE"
    );
  }
  function createHttp2Error(code, message) {
    return createErrorWithCode(message, code);
  }
  function createHttp2SettingRangeError(setting, value) {
    const error = new RangeError(
      `Invalid value for setting "${setting}": ${String(value)}`
    );
    error.code = "ERR_HTTP2_INVALID_SETTING_VALUE";
    return error;
  }
  function createHttp2SettingTypeError(setting, value) {
    const error = new TypeError(
      `Invalid value for setting "${setting}": ${String(value)}`
    );
    error.code = "ERR_HTTP2_INVALID_SETTING_VALUE";
    return error;
  }
  var HTTP2_INTERNAL_BINDING_CONSTANTS = {
    NGHTTP2_NO_ERROR: 0,
    NGHTTP2_PROTOCOL_ERROR: 1,
    NGHTTP2_INTERNAL_ERROR: 2,
    NGHTTP2_FLOW_CONTROL_ERROR: 3,
    NGHTTP2_SETTINGS_TIMEOUT: 4,
    NGHTTP2_STREAM_CLOSED: 5,
    NGHTTP2_FRAME_SIZE_ERROR: 6,
    NGHTTP2_REFUSED_STREAM: 7,
    NGHTTP2_CANCEL: 8,
    NGHTTP2_COMPRESSION_ERROR: 9,
    NGHTTP2_CONNECT_ERROR: 10,
    NGHTTP2_ENHANCE_YOUR_CALM: 11,
    NGHTTP2_INADEQUATE_SECURITY: 12,
    NGHTTP2_HTTP_1_1_REQUIRED: 13,
    NGHTTP2_NV_FLAG_NONE: 0,
    NGHTTP2_NV_FLAG_NO_INDEX: 1,
    NGHTTP2_ERR_DEFERRED: -508,
    NGHTTP2_ERR_STREAM_ID_NOT_AVAILABLE: -509,
    NGHTTP2_ERR_STREAM_CLOSED: -510,
    NGHTTP2_ERR_INVALID_ARGUMENT: -501,
    NGHTTP2_ERR_FRAME_SIZE_ERROR: -522,
    NGHTTP2_ERR_NOMEM: -901,
    NGHTTP2_FLAG_NONE: 0,
    NGHTTP2_FLAG_END_STREAM: 1,
    NGHTTP2_FLAG_END_HEADERS: 4,
    NGHTTP2_FLAG_ACK: 1,
    NGHTTP2_FLAG_PADDED: 8,
    NGHTTP2_FLAG_PRIORITY: 32,
    NGHTTP2_DEFAULT_WEIGHT: 16,
    NGHTTP2_SETTINGS_HEADER_TABLE_SIZE: 1,
    NGHTTP2_SETTINGS_ENABLE_PUSH: 2,
    NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS: 3,
    NGHTTP2_SETTINGS_INITIAL_WINDOW_SIZE: 4,
    NGHTTP2_SETTINGS_MAX_FRAME_SIZE: 5,
    NGHTTP2_SETTINGS_MAX_HEADER_LIST_SIZE: 6,
    NGHTTP2_SETTINGS_ENABLE_CONNECT_PROTOCOL: 8
  };
  var HTTP2_NGHTTP2_ERROR_MESSAGES = {
    [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_DEFERRED]: "Data deferred",
    [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_STREAM_ID_NOT_AVAILABLE]: "Stream ID is not available",
    [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_STREAM_CLOSED]: "Stream was already closed or invalid",
    [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_INVALID_ARGUMENT]: "Invalid argument",
    [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_FRAME_SIZE_ERROR]: "Frame size error",
    [HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_NOMEM]: "Out of memory"
  };
  var NghttpError = class extends Error {
    code = "ERR_HTTP2_ERROR";
    constructor(message) {
      super(message);
      this.name = "Error";
    }
  };
  function nghttp2ErrorString(code) {
    return HTTP2_NGHTTP2_ERROR_MESSAGES[code] ?? `HTTP/2 error (${String(code)})`;
  }
  function createHttp2InvalidArgValueError(property, value) {
    return createTypeErrorWithCode(
      `The property 'options.${property}' is invalid. Received ${formatHttp2InvalidValue(value)}`,
      "ERR_INVALID_ARG_VALUE"
    );
  }
  function formatHttp2InvalidValue(value) {
    if (typeof value === "function") {
      return `[Function${value.name ? `: ${value.name}` : ": function"}]`;
    }
    if (typeof value === "symbol") {
      return value.toString();
    }
    if (Array.isArray(value)) {
      return "[]";
    }
    if (value === null) {
      return "null";
    }
    if (typeof value === "object") {
      return "{}";
    }
    return String(value);
  }
  function createHttp2PayloadForbiddenError(statusCode) {
    return createHttp2Error(
      "ERR_HTTP2_PAYLOAD_FORBIDDEN",
      `Responses with ${String(statusCode)} status must not have a payload`
    );
  }
  var S_IFMT = 61440;
  var S_IFDIR = 16384;
  var S_IFREG = 32768;
  var S_IFIFO = 4096;
  var S_IFSOCK = 49152;
  var S_IFLNK = 40960;
  function createHttp2BridgeStat(stat) {
    const atimeMs = stat.atimeMs ?? 0;
    const mtimeMs = stat.mtimeMs ?? atimeMs;
    const ctimeMs = stat.ctimeMs ?? mtimeMs;
    const birthtimeMs = stat.birthtimeMs ?? ctimeMs;
    const fileType = stat.mode & S_IFMT;
    return {
      size: stat.size,
      mode: stat.mode,
      atimeMs,
      mtimeMs,
      ctimeMs,
      birthtimeMs,
      atime: new Date(atimeMs),
      mtime: new Date(mtimeMs),
      ctime: new Date(ctimeMs),
      birthtime: new Date(birthtimeMs),
      isFile: () => fileType === S_IFREG,
      isDirectory: () => fileType === S_IFDIR,
      isFIFO: () => fileType === S_IFIFO,
      isSocket: () => fileType === S_IFSOCK,
      isSymbolicLink: () => fileType === S_IFLNK
    };
  }
  function normalizeHttp2FileResponseOptions(options) {
    const normalized = options ?? {};
    const offset = normalized.offset;
    if (offset !== void 0 && (typeof offset !== "number" || !Number.isFinite(offset))) {
      throw createHttp2InvalidArgValueError("offset", offset);
    }
    const length = normalized.length;
    if (length !== void 0 && (typeof length !== "number" || !Number.isFinite(length))) {
      throw createHttp2InvalidArgValueError("length", length);
    }
    const statCheck = normalized.statCheck;
    if (statCheck !== void 0 && typeof statCheck !== "function") {
      throw createHttp2InvalidArgValueError("statCheck", statCheck);
    }
    const onError = normalized.onError;
    return {
      offset: offset === void 0 ? 0 : Math.max(0, Math.trunc(offset)),
      length: typeof length === "number" ? Math.trunc(length) : void 0,
      statCheck: typeof statCheck === "function" ? statCheck : void 0,
      onError: typeof onError === "function" ? onError : void 0
    };
  }
  function sliceHttp2FileBody(body, offset, length) {
    const safeOffset = Math.max(0, Math.min(offset, body.length));
    if (length === void 0 || length < 0) {
      return body.subarray(safeOffset);
    }
    return body.subarray(safeOffset, Math.min(body.length, safeOffset + length));
  }
  var Http2Stream = class {
    constructor(_streamId) {
      this._streamId = _streamId;
    }
    _streamId;
    respond(headers) {
      if (typeof _networkHttp2StreamRespondRaw === "undefined") {
        throw new Error("http2 server stream respond bridge is not available");
      }
      _networkHttp2StreamRespondRaw.applySync(void 0, [
        this._streamId,
        serializeHttp2Headers(headers)
      ]);
      return 0;
    }
  };
  var DEFAULT_HTTP2_SETTINGS = {
    headerTableSize: 4096,
    enablePush: true,
    initialWindowSize: 65535,
    maxFrameSize: 16384,
    maxConcurrentStreams: 4294967295,
    maxHeaderListSize: 65535,
    maxHeaderSize: 65535,
    enableConnectProtocol: false
  };
  var DEFAULT_HTTP2_SESSION_STATE = {
    effectiveLocalWindowSize: 65535,
    localWindowSize: 65535,
    remoteWindowSize: 65535,
    nextStreamID: 1,
    outboundQueueSize: 1,
    deflateDynamicTableSize: 0,
    inflateDynamicTableSize: 0
  };
  function cloneHttp2Settings(settings) {
    const cloned = {};
    for (const [key, value] of Object.entries(settings ?? {})) {
      if (key === "customSettings" && value && typeof value === "object") {
        const customSettings = {};
        for (const [customKey, customValue] of Object.entries(value)) {
          customSettings[Number(customKey)] = Number(customValue);
        }
        cloned.customSettings = customSettings;
        continue;
      }
      cloned[key] = value;
    }
    return cloned;
  }
  function cloneHttp2SessionRuntimeState(state) {
    return {
      ...DEFAULT_HTTP2_SESSION_STATE,
      ...state ?? {}
    };
  }
  function parseHttp2SessionRuntimeState(state) {
    if (!state || typeof state !== "object") {
      return void 0;
    }
    const record = state;
    const parsed = {};
    const numericKeys = [
      "effectiveLocalWindowSize",
      "localWindowSize",
      "remoteWindowSize",
      "nextStreamID",
      "outboundQueueSize",
      "deflateDynamicTableSize",
      "inflateDynamicTableSize"
    ];
    for (const key of numericKeys) {
      if (typeof record[key] === "number") {
        parsed[key] = record[key];
      }
    }
    return parsed;
  }
  function validateHttp2Settings(settings, argumentName = "settings") {
    if (!settings || typeof settings !== "object" || Array.isArray(settings)) {
      throw createHttp2ArgTypeError(argumentName, "object", settings);
    }
    const record = settings;
    const normalized = {};
    const numberRanges = {
      headerTableSize: [0, 4294967295],
      initialWindowSize: [0, 4294967295],
      maxFrameSize: [16384, 16777215],
      maxConcurrentStreams: [0, 4294967295],
      maxHeaderListSize: [0, 4294967295],
      maxHeaderSize: [0, 4294967295]
    };
    for (const [key, value] of Object.entries(record)) {
      if (value === void 0) {
        continue;
      }
      if (key === "enablePush" || key === "enableConnectProtocol") {
        if (typeof value !== "boolean") {
          throw createHttp2SettingTypeError(key, value);
        }
        normalized[key] = value;
        continue;
      }
      if (key === "customSettings") {
        if (!value || typeof value !== "object" || Array.isArray(value)) {
          throw createHttp2SettingRangeError(key, value);
        }
        const customSettings = {};
        for (const [customKey, customValue] of Object.entries(value)) {
          const numericKey = Number(customKey);
          if (!Number.isInteger(numericKey) || numericKey < 0 || numericKey > 65535) {
            throw createHttp2SettingRangeError(key, value);
          }
          if (typeof customValue !== "number" || !Number.isInteger(customValue) || customValue < 0 || customValue > 4294967295) {
            throw createHttp2SettingRangeError(key, value);
          }
          customSettings[numericKey] = customValue;
        }
        normalized.customSettings = customSettings;
        continue;
      }
      if (key in numberRanges) {
        const [min, max] = numberRanges[key];
        if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) {
          throw createHttp2SettingRangeError(key, value);
        }
        normalized[key] = value;
        continue;
      }
      normalized[key] = value;
    }
    return normalized;
  }
  function serializeHttp2Headers(headers) {
    return JSON.stringify(headers ?? {});
  }
  function parseHttp2Headers(headersJson) {
    if (!headersJson) {
      return {};
    }
    try {
      const parsed = JSON.parse(headersJson);
      return parsed && typeof parsed === "object" ? parsed : {};
    } catch {
      return {};
    }
  }
  function parseHttp2SessionState(data) {
    if (!data) {
      return null;
    }
    try {
      const parsed = JSON.parse(data);
      return parsed && typeof parsed === "object" ? parsed : null;
    } catch {
      return null;
    }
  }
  function parseHttp2SocketState(data) {
    if (!data) {
      return null;
    }
    try {
      const parsed = JSON.parse(data);
      return parsed && typeof parsed === "object" ? parsed : null;
    } catch {
      return null;
    }
  }
  function parseHttp2ErrorPayload(data) {
    if (!data) {
      return new Error("Unknown HTTP/2 bridge error");
    }
    try {
      const parsed = JSON.parse(data);
      const error = new Error(parsed.message ?? "Unknown HTTP/2 bridge error");
      if (parsed.name) error.name = parsed.name;
      if (parsed.code) error.code = parsed.code;
      return error;
    } catch {
      return new Error(data);
    }
  }
  function normalizeHttp2Headers(headers) {
    const normalized = {};
    if (!headers || typeof headers !== "object") {
      return normalized;
    }
    for (const [key, value] of Object.entries(headers)) {
      normalized[String(key)] = value;
    }
    return normalized;
  }
  function validateHttp2RequestOptions(options) {
    if (!options) {
      return;
    }
    const validators = {
      endStream: "boolean",
      weight: "number",
      parent: "number",
      exclusive: "boolean",
      silent: "boolean"
    };
    for (const [key, expectedType] of Object.entries(validators)) {
      if (!(key in options) || options[key] === void 0) {
        continue;
      }
      const value = options[key];
      if (expectedType === "boolean" && typeof value !== "boolean") {
        throw createHttp2ArgTypeError(key, "boolean", value);
      }
      if (expectedType === "number" && typeof value !== "number") {
        throw createHttp2ArgTypeError(key, "number", value);
      }
    }
  }
  function validateHttp2ConnectOptions(options) {
    if (!options || !options.settings || typeof options.settings !== "object") {
      return;
    }
    const settings = options.settings;
    if ("maxFrameSize" in settings) {
      const value = settings.maxFrameSize;
      if (typeof value !== "number" || !Number.isInteger(value) || value < 16384 || value > 16777215) {
        throw createHttp2SettingRangeError("maxFrameSize", value);
      }
    }
  }
  function applyHttp2SessionState(session, state) {
    if (!state) {
      return;
    }
    session.encrypted = state.encrypted === true;
    session.alpnProtocol = state.alpnProtocol ?? (session.encrypted ? "h2" : "h2c");
    session.originSet = Array.isArray(state.originSet) && state.originSet.length > 0 ? [...state.originSet] : session.encrypted ? [] : void 0;
    if (state.localSettings && typeof state.localSettings === "object") {
      session.localSettings = cloneHttp2Settings(state.localSettings);
    }
    if (state.remoteSettings && typeof state.remoteSettings === "object") {
      session.remoteSettings = cloneHttp2Settings(state.remoteSettings);
    }
    if (state.state && typeof state.state === "object") {
      session._applyRuntimeState(parseHttp2SessionRuntimeState(state.state));
    }
    session.socket._applyState(state.socket);
  }
  function normalizeHttp2Authority(authority, options) {
    if (authority instanceof URL) {
      return authority;
    }
    if (typeof authority === "string") {
      return new URL(authority);
    }
    if (authority && typeof authority === "object") {
      const record = authority;
      const protocol = typeof (options?.protocol ?? record.protocol) === "string" ? String(options?.protocol ?? record.protocol) : "http:";
      const hostname = typeof (options?.host ?? record.host ?? options?.hostname ?? record.hostname) === "string" ? String(options?.host ?? record.host ?? options?.hostname ?? record.hostname) : "localhost";
      const portValue = options?.port ?? record.port;
      const port = portValue === void 0 ? "" : String(portValue);
      return new URL(`${protocol}//${hostname}${port ? `:${port}` : ""}`);
    }
    return new URL("http://localhost");
  }
  function normalizeHttp2ConnectArgs(authorityOrOptions, optionsOrListener, maybeListener) {
    const listener = typeof optionsOrListener === "function" ? optionsOrListener : typeof maybeListener === "function" ? maybeListener : void 0;
    const options = typeof optionsOrListener === "function" ? {} : optionsOrListener ?? {};
    return {
      authority: normalizeHttp2Authority(authorityOrOptions, options),
      options,
      listener
    };
  }
  function resolveHttp2SocketId(socket) {
    if (!socket || typeof socket !== "object") {
      return void 0;
    }
    const value = socket._socketId;
    return typeof value === "number" && Number.isFinite(value) ? value : void 0;
  }
  var ClientHttp2Stream = class extends Http2EventEmitter {
    _streamId;
    _encoding;
    _utf8Remainder;
    _isPushStream;
    _session;
    _receivedResponse = false;
    _needsDrain = false;
    _pendingWritableBytes = 0;
    _drainScheduled = false;
    _writableHighWaterMark = 16 * 1024;
    rstCode = 0;
    readable = true;
    writable = true;
    writableEnded = false;
    writableFinished = false;
    destroyed = false;
    _writableState = { ended: false, finished: false, objectMode: false, corked: 0, length: 0 };
    constructor(streamId, session, isPushStream = false) {
      super();
      this._streamId = streamId;
      this._session = session;
      this._isPushStream = isPushStream;
      if (!isPushStream) {
        queueMicrotask(() => {
          this.emit("ready");
        });
      }
    }
    setEncoding(encoding) {
      this._encoding = encoding;
      this._utf8Remainder = this._encoding === "utf8" || this._encoding === "utf-8" ? Buffer.alloc(0) : void 0;
      return this;
    }
    close() {
      this.end();
      return this;
    }
    destroy(error) {
      if (this.destroyed) {
        return this;
      }
      this.destroyed = true;
      if (error) {
        this.emit("error", error);
      }
      this.end();
      return this;
    }
    _scheduleDrain() {
      if (!this._needsDrain || this._drainScheduled) {
        return;
      }
      this._drainScheduled = true;
      queueMicrotask(() => {
        this._drainScheduled = false;
        if (!this._needsDrain) {
          return;
        }
        this._needsDrain = false;
        this._pendingWritableBytes = 0;
        this.emit("drain");
      });
    }
    write(data, encodingOrCallback, callback) {
      if (typeof _networkHttp2StreamWriteRaw === "undefined") {
        throw new Error("http2 session stream write bridge is not available");
      }
      const buffer = Buffer.isBuffer(data) ? data : typeof data === "string" ? Buffer.from(data, typeof encodingOrCallback === "string" ? encodingOrCallback : "utf8") : Buffer.from(data);
      const wrote = _networkHttp2StreamWriteRaw.applySync(void 0, [this._streamId, buffer.toString("base64")]);
      this._pendingWritableBytes += buffer.byteLength;
      const shouldBackpressure = wrote === false || this._pendingWritableBytes >= this._writableHighWaterMark;
      if (shouldBackpressure) {
        this._needsDrain = true;
      }
      const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      cb?.();
      return !shouldBackpressure;
    }
    end(data) {
      if (typeof _networkHttp2StreamEndRaw === "undefined") {
        throw new Error("http2 session stream end bridge is not available");
      }
      let encoded = null;
      if (data !== void 0) {
        const buffer = Buffer.isBuffer(data) ? data : typeof data === "string" ? Buffer.from(data) : Buffer.from(data);
        encoded = buffer.toString("base64");
      }
      _networkHttp2StreamEndRaw.applySync(void 0, [this._streamId, encoded]);
      this.writableEnded = true;
      this._writableState.ended = true;
      queueMicrotask(() => {
        this.writable = false;
        this.writableFinished = true;
        this._writableState.finished = true;
        this.emit("finish");
      });
      return this;
    }
    resume() {
      return this;
    }
    _emitPush(headers, flags) {
      if (process.env.SECURE_EXEC_DEBUG_HTTP2_BRIDGE === "1") {
        console.error("[secure-exec http2 isolate] push", this._streamId);
      }
      this.emit("push", headers, flags ?? 0);
    }
    _hasReceivedResponse() {
      return this._receivedResponse;
    }
    _belongsTo(session) {
      return this._session === session;
    }
    _emitResponseHeaders(headers) {
      this._receivedResponse = true;
      if (process.env.SECURE_EXEC_DEBUG_HTTP2_BRIDGE === "1") {
        console.error("[secure-exec http2 isolate] response headers", this._streamId, this._isPushStream);
      }
      if (!this._isPushStream) {
        this.emit("response", headers);
      }
    }
    _emitDataChunk(dataBase64) {
      if (!dataBase64) {
        return;
      }
      const chunkBuffer = Buffer.from(dataBase64, "base64");
      if (this._utf8Remainder !== void 0) {
        const buffer = this._utf8Remainder.length > 0 ? Buffer.concat([this._utf8Remainder, chunkBuffer]) : chunkBuffer;
        const completeLength = getCompleteUtf8PrefixLength(buffer);
        const chunk = buffer.subarray(0, completeLength).toString("utf8");
        this._utf8Remainder = completeLength < buffer.length ? buffer.subarray(completeLength) : Buffer.alloc(0);
        if (chunk.length > 0) {
          this.emit("data", chunk);
        }
      } else if (this._encoding) {
        this.emit("data", chunkBuffer.toString(this._encoding));
      } else {
        this.emit("data", chunkBuffer);
      }
      this._scheduleDrain();
    }
    _emitEnd() {
      if (this._utf8Remainder && this._utf8Remainder.length > 0) {
        const trailing = this._utf8Remainder.toString("utf8");
        this._utf8Remainder = Buffer.alloc(0);
        if (trailing.length > 0) {
          this.emit("data", trailing);
        }
      }
      this.readable = false;
      this.emit("end");
      this._scheduleDrain();
    }
    _emitClose(rstCode) {
      if (typeof rstCode === "number") {
        this.rstCode = rstCode;
      }
      this.destroyed = true;
      this.readable = false;
      this.writable = false;
      this._scheduleDrain();
      this.emit("close");
    }
  };
  function getCompleteUtf8PrefixLength(buffer) {
    if (buffer.length === 0) {
      return 0;
    }
    let continuationCount = 0;
    for (let index = buffer.length - 1; index >= 0 && continuationCount < 3; index -= 1) {
      if ((buffer[index] & 192) !== 128) {
        const trailingBytes = buffer.length - index;
        const lead = buffer[index];
        const expectedBytes = (lead & 128) === 0 ? 1 : (lead & 224) === 192 ? 2 : (lead & 240) === 224 ? 3 : (lead & 248) === 240 ? 4 : 1;
        return trailingBytes < expectedBytes ? index : buffer.length;
      }
      continuationCount += 1;
    }
    return continuationCount > 0 ? buffer.length - continuationCount : buffer.length;
  }
  var ServerHttp2Stream = class _ServerHttp2Stream extends Http2EventEmitter {
    _streamId;
    _binding;
    _responded = false;
    _endQueued = false;
    _pendingSyntheticErrorSuppressions = 0;
    _requestHeaders;
    _isPushStream;
    session;
    rstCode = 0;
    readable = true;
    writable = true;
    destroyed = false;
    _readableState;
    _writableState;
    constructor(streamId, session, requestHeaders, isPushStream = false) {
      super();
      this._streamId = streamId;
      this._binding = new Http2Stream(streamId);
      this.session = session;
      this._requestHeaders = requestHeaders;
      this._isPushStream = isPushStream;
      this._readableState = {
        flowing: null,
        ended: false,
        highWaterMark: 16 * 1024
      };
      this._writableState = {
        ended: requestHeaders?.[":method"] === "HEAD"
      };
    }
    _closeWithCode(code) {
      this.rstCode = code;
      _networkHttp2StreamCloseRaw?.applySync(void 0, [this._streamId, code]);
    }
    _markSyntheticClose() {
      this.destroyed = true;
      this.readable = false;
      this.writable = false;
    }
    _shouldSuppressHostError() {
      if (this._pendingSyntheticErrorSuppressions <= 0) {
        return false;
      }
      this._pendingSyntheticErrorSuppressions -= 1;
      return true;
    }
    _emitNghttp2Error(errorCode) {
      const error = new NghttpError(nghttp2ErrorString(errorCode));
      this._pendingSyntheticErrorSuppressions += 1;
      this._markSyntheticClose();
      this.emit("error", error);
      this._closeWithCode(HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_INTERNAL_ERROR);
    }
    _emitInternalStreamError() {
      const error = createHttp2Error(
        "ERR_HTTP2_STREAM_ERROR",
        "Stream closed with error code NGHTTP2_INTERNAL_ERROR"
      );
      this._pendingSyntheticErrorSuppressions += 1;
      this._markSyntheticClose();
      this.emit("error", error);
      this._closeWithCode(HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_INTERNAL_ERROR);
    }
    _submitResponse(headers) {
      this._responded = true;
      const ngError = this._binding.respond(headers);
      if (typeof ngError === "number" && ngError !== 0) {
        this._emitNghttp2Error(ngError);
        return false;
      }
      return true;
    }
    respond(headers) {
      if (this.destroyed) {
        throw createHttp2Error("ERR_HTTP2_INVALID_STREAM", "The stream has been destroyed");
      }
      if (this._responded) {
        throw createHttp2Error("ERR_HTTP2_HEADERS_SENT", "Response has already been initiated.");
      }
      this._submitResponse(headers);
    }
    pushStream(headers, optionsOrCallback, maybeCallback) {
      if (this._isPushStream) {
        throw createHttp2Error(
          "ERR_HTTP2_NESTED_PUSH",
          "A push stream cannot initiate another push stream."
        );
      }
      const callback = typeof optionsOrCallback === "function" ? optionsOrCallback : maybeCallback;
      if (typeof callback !== "function") {
        throw createHttp2ArgTypeError("callback", "function", callback);
      }
      if (typeof _networkHttp2StreamPushStreamRaw === "undefined") {
        throw new Error("http2 server stream push bridge is not available");
      }
      const options = optionsOrCallback && typeof optionsOrCallback === "object" && !Array.isArray(optionsOrCallback) ? optionsOrCallback : {};
      const resultJson = _networkHttp2StreamPushStreamRaw.applySync(
        void 0,
        [
          this._streamId,
          serializeHttp2Headers(normalizeHttp2Headers(headers)),
          JSON.stringify(options ?? {})
        ]
      );
      const result = JSON.parse(resultJson);
      if (result.error) {
        callback(parseHttp2ErrorPayload(result.error));
        return;
      }
      const pushStream = new _ServerHttp2Stream(
        Number(result.streamId),
        this.session,
        parseHttp2Headers(result.headers),
        true
      );
      http2Streams.set(Number(result.streamId), pushStream);
      callback(null, pushStream, parseHttp2Headers(result.headers));
    }
    write(data) {
      if (this._writableState.ended) {
        queueMicrotask(() => {
          this.emit("error", createHttp2Error("ERR_STREAM_WRITE_AFTER_END", "write after end"));
        });
        return false;
      }
      if (typeof _networkHttp2StreamWriteRaw === "undefined") {
        throw new Error("http2 server stream write bridge is not available");
      }
      const buffer = Buffer.isBuffer(data) ? data : typeof data === "string" ? Buffer.from(data) : Buffer.from(data);
      return _networkHttp2StreamWriteRaw.applySync(void 0, [this._streamId, buffer.toString("base64")]);
    }
    end(data) {
      if (!this._responded) {
        if (!this._submitResponse({ ":status": 200 })) {
          return;
        }
      }
      if (this._endQueued) {
        return;
      }
      if (typeof _networkHttp2StreamEndRaw === "undefined") {
        throw new Error("http2 server stream end bridge is not available");
      }
      this._writableState.ended = true;
      let encoded = null;
      if (data !== void 0) {
        const buffer = Buffer.isBuffer(data) ? data : typeof data === "string" ? Buffer.from(data) : Buffer.from(data);
        encoded = buffer.toString("base64");
      }
      this._endQueued = true;
      queueMicrotask(() => {
        if (!this._endQueued || this.destroyed) {
          return;
        }
        this._endQueued = false;
        _networkHttp2StreamEndRaw.applySync(void 0, [this._streamId, encoded]);
      });
    }
    pause() {
      this._readableState.flowing = false;
      _networkHttp2StreamPauseRaw?.applySync(void 0, [this._streamId]);
      return this;
    }
    resume() {
      this._readableState.flowing = true;
      _networkHttp2StreamResumeRaw?.applySync(void 0, [this._streamId]);
      return this;
    }
    respondWithFile(path, headers, options) {
      if (this.destroyed) {
        throw createHttp2Error("ERR_HTTP2_INVALID_STREAM", "The stream has been destroyed");
      }
      if (this._responded) {
        throw createHttp2Error("ERR_HTTP2_HEADERS_SENT", "Response has already been initiated.");
      }
      const normalizedOptions = normalizeHttp2FileResponseOptions(options);
      const responseHeaders = { ...headers ?? {} };
      const statusCode = responseHeaders[":status"];
      if (statusCode === 204 || statusCode === 205 || statusCode === 304) {
        throw createHttp2PayloadForbiddenError(Number(statusCode));
      }
      try {
        const statJson = _fs.stat.applySyncPromise(void 0, [path]);
        const bodyBase64 = _fs.readFileBinary.applySyncPromise(void 0, [path]);
        const stat = createHttp2BridgeStat(decodeBridgeJson(statJson));
        const callbackOptions = {
          offset: normalizedOptions.offset,
          length: normalizedOptions.length ?? Math.max(0, stat.size - normalizedOptions.offset)
        };
        normalizedOptions.statCheck?.(stat, responseHeaders, callbackOptions);
        const body = Buffer.from(bodyBase64, "base64");
        const slicedBody = sliceHttp2FileBody(
          body,
          normalizedOptions.offset,
          normalizedOptions.length
        );
        if (responseHeaders["content-length"] === void 0) {
          responseHeaders["content-length"] = slicedBody.byteLength;
        }
        if (!this._submitResponse({
          ":status": 200,
          ...responseHeaders
        })) {
          return;
        }
        this.end(slicedBody);
        return;
      } catch {
      }
      if (typeof _networkHttp2StreamRespondWithFileRaw === "undefined") {
        throw new Error("http2 server stream respondWithFile bridge is not available");
      }
      this._responded = true;
      _networkHttp2StreamRespondWithFileRaw.applySync(
        void 0,
        [
          this._streamId,
          path,
          JSON.stringify(headers ?? {}),
          JSON.stringify(options ?? {})
        ]
      );
    }
    respondWithFD(fdOrHandle, headers, options) {
      const fd = typeof fdOrHandle === "number" ? fdOrHandle : typeof fdOrHandle?.fd === "number" ? fdOrHandle.fd : NaN;
      const path = Number.isFinite(fd) ? _fdGetPath.applySync(void 0, [fd]) : null;
      if (!path) {
        this._emitInternalStreamError();
        return;
      }
      this.respondWithFile(path, headers, options);
    }
    destroy(error) {
      if (this.destroyed) {
        return this;
      }
      this.destroyed = true;
      if (error) {
        this.emit("error", error);
      }
      this._closeWithCode(HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_CANCEL);
      return this;
    }
    _emitData(dataBase64) {
      if (!dataBase64) {
        return;
      }
      this.emit("data", Buffer.from(dataBase64, "base64"));
    }
    _emitEnd() {
      this._readableState.ended = true;
      this.emit("end");
    }
    _emitDrain() {
      this.emit("drain");
    }
    _emitClose(rstCode) {
      if (typeof rstCode === "number") {
        this.rstCode = rstCode;
      }
      this.destroyed = true;
      this.emit("close");
    }
  };
  var Http2ServerRequest = class extends Http2EventEmitter {
    headers;
    method;
    url;
    connection;
    socket;
    stream;
    destroyed = false;
    readable = true;
    _readableState = { flowing: null, length: 0, ended: false, objectMode: false };
    constructor(headers, socket, stream) {
      super();
      this.headers = headers;
      this.method = typeof headers[":method"] === "string" ? String(headers[":method"]) : "GET";
      this.url = typeof headers[":path"] === "string" ? String(headers[":path"]) : "/";
      this.connection = socket;
      this.socket = socket;
      this.stream = stream;
    }
    on(event, listener) {
      super.on(event, listener);
      if (event === "data" && this._readableState.flowing !== false) {
        this.resume();
      }
      return this;
    }
    once(event, listener) {
      super.once(event, listener);
      if (event === "data" && this._readableState.flowing !== false) {
        this.resume();
      }
      return this;
    }
    resume() {
      this._readableState.flowing = true;
      this.stream.resume();
      return this;
    }
    pause() {
      this._readableState.flowing = false;
      this.stream.pause();
      return this;
    }
    pipe(dest) {
      this.on("data", (chunk) => {
        const wrote = dest.write(chunk);
        if (wrote === false && typeof dest.once === "function") {
          this.pause();
          dest.once("drain", () => this.resume());
        }
      });
      this.on("end", () => dest.end());
      this.resume();
      return dest;
    }
    unpipe() {
      return this;
    }
    read() {
      return null;
    }
    isPaused() {
      return this._readableState.flowing === false;
    }
    setEncoding() {
      return this;
    }
    _emitData(chunk) {
      this._readableState.length += chunk.byteLength;
      this.emit("data", chunk);
    }
    _emitEnd() {
      this._readableState.ended = true;
      this.emit("end");
      this.emit("close");
    }
    _emitError(error) {
      this.emit("error", error);
    }
    destroy(err) {
      this.destroyed = true;
      if (err) {
        this.emit("error", err);
      }
      this.emit("close");
      return this;
    }
  };
  var Http2ServerResponse = class extends Http2EventEmitter {
    _stream;
    _headers = {};
    _statusCode = 200;
    headersSent = false;
    writable = true;
    writableEnded = false;
    writableFinished = false;
    socket;
    connection;
    stream;
    _writableState = { ended: false, finished: false, objectMode: false, corked: 0, length: 0 };
    constructor(stream) {
      super();
      this._stream = stream;
      this.stream = stream;
      this.socket = stream.session.socket;
      this.connection = this.socket;
    }
    writeHead(statusCode, headers) {
      this._statusCode = statusCode;
      this._headers = {
        ...this._headers,
        ...headers ?? {},
        ":status": statusCode
      };
      this._stream.respond(this._headers);
      this.headersSent = true;
      return this;
    }
    setHeader(name, value) {
      this._headers[name] = value;
      return this;
    }
    getHeader(name) {
      return this._headers[name];
    }
    hasHeader(name) {
      return Object.prototype.hasOwnProperty.call(this._headers, name);
    }
    removeHeader(name) {
      delete this._headers[name];
    }
    write(data, encodingOrCallback, callback) {
      if (!(":status" in this._headers)) {
        this._headers[":status"] = this._statusCode;
        this._stream.respond(this._headers);
        this.headersSent = true;
      }
      const wrote = this._stream.write(
        typeof data === "string" && typeof encodingOrCallback === "string" ? Buffer.from(data, encodingOrCallback) : data
      );
      const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      cb?.();
      return wrote;
    }
    end(data) {
      if (!(":status" in this._headers)) {
        this._headers[":status"] = this._statusCode;
        this._stream.respond(this._headers);
        this.headersSent = true;
      }
      this.writableEnded = true;
      this._writableState.ended = true;
      this._stream.end(data);
      queueMicrotask(() => {
        this.writable = false;
        this.writableFinished = true;
        this._writableState.finished = true;
        this.emit("finish");
        this.emit("close");
      });
      return this;
    }
    destroy(err) {
      if (err) {
        this.emit("error", err);
      }
      this.writable = false;
      this.writableEnded = true;
      this.writableFinished = true;
      this.emit("close");
      return this;
    }
  };
  var Http2Session = class extends Http2EventEmitter {
    encrypted = false;
    alpnProtocol = false;
    originSet;
    localSettings = cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
    remoteSettings = cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
    pendingSettingsAck = false;
    socket;
    state = cloneHttp2SessionRuntimeState(DEFAULT_HTTP2_SESSION_STATE);
    _sessionId;
    _waitStarted = false;
    _pendingSettingsAckCount = 0;
    _awaitingInitialSettingsAck = false;
    _settingsCallbacks = [];
    constructor(sessionId, socketState) {
      super();
      this._sessionId = sessionId;
      this.socket = new Http2SocketProxy(socketState, () => {
        setTimeout(() => {
          this.destroy();
        }, 0);
      });
      this[HTTP2_K_SOCKET] = this.socket;
    }
    _retain() {
      if (this._waitStarted || typeof _networkHttp2SessionWaitRaw === "undefined") {
        return;
      }
      this._waitStarted = true;
      void _networkHttp2SessionWaitRaw.apply(void 0, [this._sessionId], {
        result: { promise: true }
      }).catch((error) => {
        this.emit("error", error instanceof Error ? error : new Error(String(error)));
      });
    }
    _release() {
      this._waitStarted = false;
    }
    _beginInitialSettingsAck() {
      this._awaitingInitialSettingsAck = true;
      this._pendingSettingsAckCount += 1;
      this.pendingSettingsAck = true;
    }
    _applyLocalSettings(settings) {
      this.localSettings = cloneHttp2Settings(settings);
      if (this._awaitingInitialSettingsAck) {
        this._awaitingInitialSettingsAck = false;
        this._pendingSettingsAckCount = Math.max(0, this._pendingSettingsAckCount - 1);
        this.pendingSettingsAck = this._pendingSettingsAckCount > 0;
      }
      this.emit("localSettings", this.localSettings);
    }
    _applyRemoteSettings(settings) {
      this.remoteSettings = cloneHttp2Settings(settings);
      this.emit("remoteSettings", this.remoteSettings);
    }
    _applyRuntimeState(state) {
      this.state = cloneHttp2SessionRuntimeState(state);
    }
    _ackSettings() {
      this._pendingSettingsAckCount = Math.max(0, this._pendingSettingsAckCount - 1);
      this.pendingSettingsAck = this._pendingSettingsAckCount > 0;
      const callback = this._settingsCallbacks.shift();
      callback?.();
    }
    request(headers, options) {
      if (typeof _networkHttp2SessionRequestRaw === "undefined") {
        throw new Error("http2 session request bridge is not available");
      }
      validateHttp2RequestOptions(options);
      const streamId = _networkHttp2SessionRequestRaw.applySync(
        void 0,
        [
          this._sessionId,
          serializeHttp2Headers(normalizeHttp2Headers(headers)),
          JSON.stringify(options ?? {})
        ]
      );
      const stream = new ClientHttp2Stream(streamId, this);
      http2Streams.set(streamId, stream);
      return stream;
    }
    settings(settings, callback) {
      if (callback !== void 0 && typeof callback !== "function") {
        throw createHttp2ArgTypeError("callback", "function", callback);
      }
      if (typeof _networkHttp2SessionSettingsRaw === "undefined") {
        throw new Error("http2 session settings bridge is not available");
      }
      const normalized = validateHttp2Settings(settings);
      _networkHttp2SessionSettingsRaw.applySync(
        void 0,
        [this._sessionId, JSON.stringify(normalized)]
      );
      this._pendingSettingsAckCount += 1;
      this.pendingSettingsAck = true;
      if (callback) {
        this._settingsCallbacks.push(callback);
      }
    }
    setLocalWindowSize(windowSize) {
      if (typeof windowSize !== "number" || Number.isNaN(windowSize)) {
        throw createHttp2ArgTypeError("windowSize", "number", windowSize);
      }
      if (!Number.isInteger(windowSize) || windowSize < 0 || windowSize > 2147483647) {
        const error = new RangeError(
          `The value of "windowSize" is out of range. It must be >= 0 && <= 2147483647. Received ${windowSize}`
        );
        error.code = "ERR_OUT_OF_RANGE";
        throw error;
      }
      if (typeof _networkHttp2SessionSetLocalWindowSizeRaw === "undefined") {
        throw new Error("http2 session setLocalWindowSize bridge is not available");
      }
      const result = _networkHttp2SessionSetLocalWindowSizeRaw.applySync(
        void 0,
        [this._sessionId, windowSize]
      );
      this._applyRuntimeState(parseHttp2SessionState(result)?.state);
    }
    goaway(code = 0, lastStreamID = 0, opaqueData) {
      const payload = opaqueData === void 0 ? null : Buffer.isBuffer(opaqueData) ? opaqueData.toString("base64") : typeof opaqueData === "string" ? Buffer.from(opaqueData).toString("base64") : Buffer.from(opaqueData).toString("base64");
      _networkHttp2SessionGoawayRaw?.applySync(void 0, [this._sessionId, code, lastStreamID, payload]);
    }
    close() {
      const pendingStreams = Array.from(http2Streams.entries()).filter(
        ([, stream]) => typeof stream._belongsTo === "function" && stream._belongsTo(this) && !stream._hasReceivedResponse()
      );
      if (pendingStreams.length > 0) {
        const error = createHttp2Error(
          "ERR_HTTP2_GOAWAY_SESSION",
          "The HTTP/2 session is closing before the stream could be established."
        );
        queueMicrotask(() => {
          for (const [streamId, stream] of pendingStreams) {
            if (http2Streams.get(streamId) !== stream) {
              continue;
            }
            stream.emit("error", error);
            stream.emit("close");
            http2Streams.delete(streamId);
          }
        });
        if (typeof _networkHttp2SessionDestroyRaw !== "undefined") {
          _networkHttp2SessionDestroyRaw.applySync(void 0, [this._sessionId]);
          return;
        }
      }
      _networkHttp2SessionCloseRaw?.applySync(void 0, [this._sessionId]);
      setTimeout(() => {
        if (!http2Sessions.has(this._sessionId)) {
          return;
        }
        this._release();
        this.emit("close");
        http2Sessions.delete(this._sessionId);
        _unregisterHandle?.(`http2:session:${this._sessionId}`);
      }, 50);
    }
    destroy() {
      if (typeof _networkHttp2SessionDestroyRaw !== "undefined") {
        _networkHttp2SessionDestroyRaw.applySync(void 0, [this._sessionId]);
        return;
      }
      this.close();
    }
  };
  var Http2Server = class extends Http2EventEmitter {
    allowHalfOpen;
    allowHTTP1;
    encrypted;
    _serverId;
    listening = false;
    _address = null;
    _options;
    _timeoutMs = 0;
    _waitStarted = false;
    constructor(options, listener, encrypted) {
      super();
      this.allowHalfOpen = options?.allowHalfOpen === true;
      this.allowHTTP1 = options?.allowHTTP1 === true;
      this.encrypted = encrypted;
      const initialSettings = options?.settings && typeof options.settings === "object" && !Array.isArray(options.settings) ? cloneHttp2Settings(options.settings) : {};
      this._options = {
        ...options ?? {},
        settings: initialSettings
      };
      this._serverId = nextHttp2ServerId++;
      this[HTTP2_OPTIONS] = {
        settings: cloneHttp2Settings(initialSettings),
        unknownProtocolTimeout: 1e4,
        ...encrypted ? { ALPNProtocols: ["h2"] } : {}
      };
      if (listener) {
        this.on("request", listener);
      }
      http2Servers.set(this._serverId, this);
    }
    address() {
      return this._address;
    }
    _retain() {
      if (this._waitStarted || typeof _networkHttp2ServerWaitRaw === "undefined") {
        return;
      }
      this._waitStarted = true;
      void _networkHttp2ServerWaitRaw.apply(void 0, [this._serverId], {
        result: { promise: true }
      }).catch((error) => {
        this.emit("error", error instanceof Error ? error : new Error(String(error)));
      });
    }
    _release() {
      this._waitStarted = false;
    }
    setTimeout(timeout, callback) {
      this._timeoutMs = normalizeSocketTimeout(timeout);
      if (callback) {
        this.on("timeout", callback);
      }
      return this;
    }
    updateSettings(settings) {
      const normalized = validateHttp2Settings(settings);
      const mergedSettings = {
        ...cloneHttp2Settings(this._options.settings),
        ...cloneHttp2Settings(normalized)
      };
      this._options = {
        ...this._options,
        settings: mergedSettings
      };
      const optionsState = this[HTTP2_OPTIONS];
      optionsState.settings = cloneHttp2Settings(mergedSettings);
      return this;
    }
    listen(portOrOptions, hostOrCallback, backlogOrCallback, callback) {
      if (typeof _networkHttp2ServerListenRaw === "undefined") {
        throw new Error(`http2.${this.encrypted ? "createSecureServer" : "createServer"} is not supported in sandbox`);
      }
      const options = normalizeListenArgs(portOrOptions, hostOrCallback, backlogOrCallback, callback);
      if (options.callback) {
        this.once("listening", options.callback);
      }
      const payload = {
        serverId: this._serverId,
        secure: this.encrypted,
        port: options.port,
        host: options.host,
        backlog: options.backlog,
        allowHalfOpen: this.allowHalfOpen,
        allowHTTP1: this._options.allowHTTP1 === true,
        timeout: this._timeoutMs,
        settings: this._options.settings,
        remoteCustomSettings: this._options.remoteCustomSettings,
        tls: this.encrypted ? buildSerializedTlsOptions(
          {
            ...this._options,
            ...portOrOptions && typeof portOrOptions === "object" ? portOrOptions : {}
          },
          { isServer: true }
        ) : void 0
      };
      const result = JSON.parse(
        _networkHttp2ServerListenRaw.applySyncPromise(void 0, [JSON.stringify(payload)])
      );
      this._address = result.address ?? null;
      this.listening = true;
      this._retain();
      _registerHandle?.(`http2:server:${this._serverId}`, "http2 server");
      this.emit("listening");
      return this;
    }
    close(callback) {
      if (callback) {
        this.once("close", callback);
      }
      if (!this.listening) {
        this._release();
        queueMicrotask(() => this.emit("close"));
        return this;
      }
      void _networkHttp2ServerCloseRaw?.apply(void 0, [this._serverId], {
        result: { promise: true }
      });
      setTimeout(() => {
        if (!this.listening) {
          return;
        }
        this.listening = false;
        this._release();
        this.emit("close");
        http2Servers.delete(this._serverId);
        _unregisterHandle?.(`http2:server:${this._serverId}`);
      }, 50);
      return this;
    }
  };
  function createHttp2Server(secure, optionsOrListener, maybeListener) {
    const listener = typeof optionsOrListener === "function" ? optionsOrListener : maybeListener;
    const options = optionsOrListener && typeof optionsOrListener === "object" && !Array.isArray(optionsOrListener) ? optionsOrListener : void 0;
    return new Http2Server(options, listener, secure);
  }
  function connectHttp2(authorityOrOptions, optionsOrListener, maybeListener) {
    if (typeof _networkHttp2SessionConnectRaw === "undefined") {
      throw new Error("http2.connect is not supported in sandbox");
    }
    const { authority, options, listener } = normalizeHttp2ConnectArgs(
      authorityOrOptions,
      optionsOrListener,
      maybeListener
    );
    if (authority.protocol !== "http:" && authority.protocol !== "https:") {
      throw createHttp2Error(
        "ERR_HTTP2_UNSUPPORTED_PROTOCOL",
        `protocol "${authority.protocol}" is unsupported.`
      );
    }
    validateHttp2ConnectOptions(options);
    const socketId = options.createConnection ? resolveHttp2SocketId(options.createConnection()) : void 0;
    const response = JSON.parse(
      _networkHttp2SessionConnectRaw.applySyncPromise(
        void 0,
        [
          JSON.stringify({
            authority: authority.toString(),
            protocol: authority.protocol,
            host: options.host ?? options.hostname ?? authority.hostname,
            port: options.port ?? authority.port,
            localAddress: options.localAddress,
            family: options.family,
            socketId,
            settings: options.settings,
            remoteCustomSettings: options.remoteCustomSettings,
            tls: authority.protocol === "https:" ? buildSerializedTlsOptions(options, { servername: typeof options.servername === "string" ? options.servername : authority.hostname }) : void 0
          })
        ]
      )
    );
    const initialState = parseHttp2SessionState(response.state);
    const session = new Http2Session(
      response.sessionId,
      initialState?.socket ?? void 0
    );
    applyHttp2SessionState(session, initialState);
    session._beginInitialSettingsAck();
    session._retain();
    if (listener) {
      session.once("connect", () => listener(session));
    }
    http2Sessions.set(response.sessionId, session);
    _registerHandle?.(`http2:session:${response.sessionId}`, "http2 session");
    if (authority.protocol === "https:") {
      session.socket.once("secureConnect", () => {
      });
    }
    return session;
  }
  function getOrCreateHttp2Session(sessionId, state) {
    let session = http2Sessions.get(sessionId);
    if (!session) {
      session = new Http2Session(sessionId, state?.socket ?? void 0);
      http2Sessions.set(sessionId, session);
    }
    applyHttp2SessionState(session, state);
    return session;
  }
  function queuePendingHttp2ClientStreamEvent(streamId, event) {
    const pending = pendingHttp2ClientStreamEvents.get(streamId) ?? [];
    pending.push(event);
    pendingHttp2ClientStreamEvents.set(streamId, pending);
  }
  function schedulePendingHttp2ClientStreamEventsFlush(streamId) {
    if (scheduledHttp2ClientStreamFlushes.has(streamId)) {
      return;
    }
    scheduledHttp2ClientStreamFlushes.add(streamId);
    const flush = () => {
      scheduledHttp2ClientStreamFlushes.delete(streamId);
      flushPendingHttp2ClientStreamEvents(streamId);
    };
    const scheduleImmediate = globalThis.setImmediate;
    if (typeof scheduleImmediate === "function") {
      scheduleImmediate(flush);
      return;
    }
    setTimeout(flush, 0);
  }
  function flushPendingHttp2ClientStreamEvents(streamId) {
    const stream = http2Streams.get(streamId);
    if (!stream || typeof stream._emitResponseHeaders !== "function") {
      return;
    }
    const pending = pendingHttp2ClientStreamEvents.get(streamId);
    if (!pending || pending.length === 0) {
      return;
    }
    pendingHttp2ClientStreamEvents.delete(streamId);
    for (const event of pending) {
      if (event.kind === "push") {
        stream._emitPush(parseHttp2Headers(event.data), event.extraNumber);
        continue;
      }
      if (event.kind === "responseHeaders") {
        stream._emitResponseHeaders(parseHttp2Headers(event.data));
        continue;
      }
      if (event.kind === "data") {
        stream._emitDataChunk(event.data);
        continue;
      }
      if (event.kind === "end") {
        stream._emitEnd();
        continue;
      }
      if (event.kind === "error") {
        stream.emit("error", parseHttp2ErrorPayload(event.data));
        continue;
      }
      if (typeof stream._emitClose === "function") {
        stream._emitClose(event.extraNumber);
      } else {
        stream.emit("close");
      }
      http2Streams.delete(streamId);
    }
  }
  function http2Dispatch(kind, id, data, extra, extraNumber, extraHeaders, flags) {
    if (kind === "sessionConnect") {
      const session = http2Sessions.get(id);
      if (!session) return;
      const state = parseHttp2SessionState(data);
      applyHttp2SessionState(session, state);
      if (session.encrypted) {
        session.socket.emit("secureConnect");
      }
      session.emit("connect");
      return;
    }
    if (kind === "sessionClose") {
      const session = http2Sessions.get(id);
      if (!session) return;
      session._release();
      session.emit("close");
      http2Sessions.delete(id);
      _unregisterHandle?.(`http2:session:${id}`);
      return;
    }
    if (kind === "sessionError") {
      const session = http2Sessions.get(id);
      if (!session) return;
      session.emit("error", parseHttp2ErrorPayload(data));
      return;
    }
    if (kind === "sessionLocalSettings") {
      const session = http2Sessions.get(id);
      if (!session) return;
      session._applyLocalSettings(parseHttp2Headers(data));
      return;
    }
    if (kind === "sessionRemoteSettings") {
      const session = http2Sessions.get(id);
      if (!session) return;
      session._applyRemoteSettings(parseHttp2Headers(data));
      return;
    }
    if (kind === "sessionSettingsAck") {
      const session = http2Sessions.get(id);
      if (!session) return;
      session._ackSettings();
      return;
    }
    if (kind === "sessionGoaway") {
      const session = http2Sessions.get(id);
      if (!session) return;
      session.emit(
        "goaway",
        Number(extraNumber ?? 0),
        Number(flags ?? 0),
        data ? Buffer.from(data, "base64") : Buffer.alloc(0)
      );
      return;
    }
    if (kind === "clientPushStream") {
      const session = http2Sessions.get(id);
      if (!session) return;
      const streamId = Number(data);
      const stream = new ClientHttp2Stream(streamId, session, true);
      http2Streams.set(streamId, stream);
      session.emit("stream", stream, parseHttp2Headers(extraHeaders), Number(flags ?? 0));
      schedulePendingHttp2ClientStreamEventsFlush(streamId);
      return;
    }
    if (kind === "clientPushHeaders") {
      queuePendingHttp2ClientStreamEvent(id, {
        kind: "push",
        data,
        extraNumber: Number(extraNumber ?? 0)
      });
      schedulePendingHttp2ClientStreamEventsFlush(id);
      return;
    }
    if (kind === "clientResponseHeaders") {
      queuePendingHttp2ClientStreamEvent(id, {
        kind: "responseHeaders",
        data
      });
      schedulePendingHttp2ClientStreamEventsFlush(id);
      return;
    }
    if (kind === "clientData") {
      queuePendingHttp2ClientStreamEvent(id, {
        kind: "data",
        data
      });
      schedulePendingHttp2ClientStreamEventsFlush(id);
      return;
    }
    if (kind === "clientEnd") {
      queuePendingHttp2ClientStreamEvent(id, {
        kind: "end"
      });
      schedulePendingHttp2ClientStreamEventsFlush(id);
      return;
    }
    if (kind === "clientClose") {
      queuePendingHttp2ClientStreamEvent(id, {
        kind: "close",
        extraNumber: Number(extraNumber ?? 0)
      });
      schedulePendingHttp2ClientStreamEventsFlush(id);
      return;
    }
    if (kind === "clientError") {
      queuePendingHttp2ClientStreamEvent(id, {
        kind: "error",
        data
      });
      schedulePendingHttp2ClientStreamEventsFlush(id);
      return;
    }
    if (kind === "serverStream") {
      const server = http2Servers.get(id);
      if (!server) return;
      const sessionState = parseHttp2SessionState(extra);
      const sessionId = Number(extraNumber);
      const session = getOrCreateHttp2Session(sessionId, sessionState);
      const streamId = Number(data);
      const headers = parseHttp2Headers(extraHeaders);
      const numericFlags = Number(flags ?? 0);
      const stream = new ServerHttp2Stream(streamId, session, headers);
      http2Streams.set(streamId, stream);
      server.emit("stream", stream, headers, numericFlags);
      if (server.listenerCount("request") > 0) {
        const request = new Http2ServerRequest(headers, session.socket, stream);
        const response = new Http2ServerResponse(stream);
        stream.on("data", (chunk) => {
          request._emitData(chunk);
        });
        stream.on("end", () => {
          request._emitEnd();
        });
        stream.on("error", (error) => {
          request._emitError(error);
        });
        stream.on("drain", () => {
          response.emit("drain");
        });
        server.emit("request", request, response);
      }
      return;
    }
    if (kind === "serverStreamData") {
      const stream = http2Streams.get(id);
      if (!stream || typeof stream._emitData !== "function") return;
      stream._emitData(data);
      return;
    }
    if (kind === "serverStreamEnd") {
      const stream = http2Streams.get(id);
      if (!stream || typeof stream._emitEnd !== "function") return;
      stream._emitEnd();
      return;
    }
    if (kind === "serverStreamDrain") {
      const stream = http2Streams.get(id);
      if (!stream || typeof stream._emitDrain !== "function") return;
      stream._emitDrain();
      return;
    }
    if (kind === "serverStreamError") {
      const stream = http2Streams.get(id);
      if (!stream) return;
      if (typeof stream._shouldSuppressHostError === "function" && stream._shouldSuppressHostError()) {
        return;
      }
      stream.emit("error", parseHttp2ErrorPayload(data));
      return;
    }
    if (kind === "serverStreamClose") {
      const stream = http2Streams.get(id);
      if (!stream || typeof stream._emitClose !== "function") return;
      stream._emitClose(Number(extraNumber ?? 0));
      http2Streams.delete(id);
      return;
    }
    if (kind === "serverSession") {
      const server = http2Servers.get(id);
      if (!server) return;
      const sessionId = Number(extraNumber);
      const session = getOrCreateHttp2Session(sessionId, parseHttp2SessionState(data));
      server.emit("session", session);
      return;
    }
    if (kind === "serverTimeout") {
      http2Servers.get(id)?.emit("timeout");
      return;
    }
    if (kind === "serverConnection") {
      http2Servers.get(id)?.emit("connection", new Http2SocketProxy(parseHttp2SocketState(data) ?? void 0));
      return;
    }
    if (kind === "serverSecureConnection") {
      http2Servers.get(id)?.emit("secureConnection", new Http2SocketProxy(parseHttp2SocketState(data) ?? void 0));
      return;
    }
    if (kind === "serverClose") {
      const server = http2Servers.get(id);
      if (!server) return;
      server.listening = false;
      server._release();
      server.emit("close");
      http2Servers.delete(id);
      _unregisterHandle?.(`http2:server:${id}`);
      return;
    }
    if (kind === "serverCompatRequest") {
      pendingHttp2CompatRequests.set(Number(extraNumber), {
        serverId: id,
        requestJson: data ?? "{}"
      });
      void dispatchHttp2CompatibilityRequest(id, Number(extraNumber));
    }
  }
  function scheduleQueuedHttp2DispatchDrain() {
    if (scheduledHttp2DispatchDrain) {
      return;
    }
    scheduledHttp2DispatchDrain = true;
    const drain = () => {
      scheduledHttp2DispatchDrain = false;
      while (queuedHttp2DispatchEvents.length > 0) {
        const event = queuedHttp2DispatchEvents.shift();
        if (!event) {
          continue;
        }
        http2Dispatch(
          event.kind,
          event.id,
          event.data,
          event.extra,
          event.extraNumber,
          event.extraHeaders,
          event.flags
        );
      }
    };
    queueMicrotask(drain);
  }
  function onHttp2Dispatch(_eventType, payload) {
    if (!payload || typeof payload !== "object") {
      return;
    }
    const event = payload;
    if (typeof event.kind !== "string" || typeof event.id !== "number") {
      return;
    }
    if (process.env.SECURE_EXEC_DEBUG_HTTP2_BRIDGE === "1") {
      console.error("[secure-exec http2 isolate dispatch]", event.kind, event.id);
    }
    const kind = event.kind;
    const id = event.id;
    const data = typeof event.data === "string" ? event.data : void 0;
    const extra = typeof event.extra === "string" ? event.extra : void 0;
    const normalizedExtraNumber = typeof event.extraNumber === "string" || typeof event.extraNumber === "number" ? event.extraNumber : void 0;
    const extraHeaders = typeof event.extraHeaders === "string" ? event.extraHeaders : void 0;
    const flags = typeof event.flags === "string" || typeof event.flags === "number" ? event.flags : void 0;
    queuedHttp2DispatchEvents.push({
      kind,
      id,
      data,
      extra,
      extraNumber: normalizedExtraNumber,
      extraHeaders,
      flags
    });
    scheduleQueuedHttp2DispatchDrain();
  }
  var http2 = {
    Http2ServerRequest,
    Http2ServerResponse,
    Http2Stream,
    NghttpError,
    nghttp2ErrorString,
    constants: {
      HTTP2_HEADER_METHOD: ":method",
      HTTP2_HEADER_PATH: ":path",
      HTTP2_HEADER_SCHEME: ":scheme",
      HTTP2_HEADER_AUTHORITY: ":authority",
      HTTP2_HEADER_STATUS: ":status",
      HTTP2_HEADER_CONTENT_TYPE: "content-type",
      HTTP2_HEADER_CONTENT_LENGTH: "content-length",
      HTTP2_HEADER_LAST_MODIFIED: "last-modified",
      HTTP2_HEADER_ACCEPT: "accept",
      HTTP2_HEADER_ACCEPT_ENCODING: "accept-encoding",
      HTTP2_METHOD_GET: "GET",
      HTTP2_METHOD_POST: "POST",
      HTTP2_METHOD_PUT: "PUT",
      HTTP2_METHOD_DELETE: "DELETE",
      ...HTTP2_INTERNAL_BINDING_CONSTANTS,
      DEFAULT_SETTINGS_MAX_HEADER_LIST_SIZE: 65535
    },
    getDefaultSettings() {
      return cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
    },
    connect: connectHttp2,
    createServer: createHttp2Server.bind(void 0, false),
    createSecureServer: createHttp2Server.bind(void 0, true)
  };
  exposeCustomGlobal("_httpModule", http);
  exposeCustomGlobal("_httpsModule", https);
  exposeCustomGlobal("_http2Module", http2);
  exposeCustomGlobal("_dnsModule", dns);
  function onHttpServerRequest(eventType, payload) {
    debugBridgeNetwork("http stream event", eventType, payload);
    if (eventType !== "http_request") {
      return;
    }
    if (!payload || payload.serverId === void 0 || payload.requestId === void 0 || typeof payload.request !== "string") {
      return;
    }
    if (typeof _networkHttpServerRespondRaw === "undefined") {
      debugBridgeNetwork("http stream missing respond bridge");
      return;
    }
    void dispatchServerRequest(payload.serverId, payload.request).then((responseJson) => {
      debugBridgeNetwork("http stream response", payload.serverId, payload.requestId);
      _networkHttpServerRespondRaw.applySync(void 0, [
        payload.serverId,
        payload.requestId,
        responseJson
      ]);
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      debugBridgeNetwork("http stream error", payload.serverId, payload.requestId, message);
      _networkHttpServerRespondRaw.applySync(void 0, [
        payload.serverId,
        payload.requestId,
        JSON.stringify({
          status: 500,
          headers: [["content-type", "text/plain"]],
          body: `Error: ${message}`,
          bodyEncoding: "utf8"
        })
      ]);
    });
  }
  exposeCustomGlobal("_httpServerDispatch", onHttpServerRequest);
  exposeCustomGlobal("_httpServerUpgradeDispatch", dispatchUpgradeRequest);
  exposeCustomGlobal("_httpServerConnectDispatch", dispatchConnectRequest);
  exposeCustomGlobal("_http2Dispatch", onHttp2Dispatch);
  exposeCustomGlobal("_upgradeSocketData", onUpgradeSocketData);
  exposeCustomGlobal("_upgradeSocketEnd", onUpgradeSocketEnd);
  exposeCustomGlobal("fetch", fetch);
  exposeCustomGlobal("Headers", UndiciHeaders);
  exposeCustomGlobal("Request", UndiciRequest);
  exposeCustomGlobal("Response", UndiciResponse);
  if (typeof globalThis.Blob === "undefined") {
    exposeCustomGlobal("Blob", class BlobStub {
    });
  }
  if (typeof globalThis.File === "undefined") {
    class FileStub extends Blob {
      name;
      lastModified;
      webkitRelativePath;
      constructor(parts = [], name = "", options = {}) {
        super(parts, options);
        this.name = String(name);
        this.lastModified = typeof options.lastModified === "number" ? options.lastModified : Date.now();
        this.webkitRelativePath = "";
      }
    }
    exposeCustomGlobal("File", FileStub);
  }
  if (typeof globalThis.FormData === "undefined") {
    class FormDataStub {
      _entries = [];
      append(name, value) {
        this._entries.push([name, value]);
      }
      get(name) {
        const entry = this._entries.find(([k]) => k === name);
        return entry ? entry[1] : null;
      }
      getAll(name) {
        return this._entries.filter(([k]) => k === name).map(([, v]) => v);
      }
      has(name) {
        return this._entries.some(([k]) => k === name);
      }
      delete(name) {
        this._entries = this._entries.filter(([k]) => k !== name);
      }
      entries() {
        return this._entries[Symbol.iterator]();
      }
      [Symbol.iterator]() {
        return this.entries();
      }
    }
    exposeCustomGlobal("FormData", FormDataStub);
  }
  var NET_SOCKET_REGISTRY_PREFIX = "__secureExecNetSocket:";
  var NET_SERVER_HANDLE_PREFIX = "net-server:";
  function getRegisteredNetSocket(socketId) {
    return globalThis[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`];
  }
  function registerNetSocket(socketId, socket) {
    globalThis[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`] = socket;
  }
  function unregisterNetSocket(socketId) {
    delete globalThis[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`];
  }
  function isTruthySocketOption(value) {
    return value === void 0 ? true : Boolean(value);
  }
  function normalizeKeepAliveDelay(initialDelay) {
    if (typeof initialDelay !== "number" || !Number.isFinite(initialDelay)) {
      return 0;
    }
    return Math.max(0, Math.floor(initialDelay / 1e3));
  }
  function createTimeoutArgTypeError(argumentName, value) {
    return createTypeErrorWithCode(
      `The "${argumentName}" argument must be of type number. Received ${formatReceivedType(value)}`,
      "ERR_INVALID_ARG_TYPE"
    );
  }
  function createFunctionArgTypeError(argumentName, value) {
    return createTypeErrorWithCode(
      `The "${argumentName}" argument must be of type function. Received ${formatReceivedType(value)}`,
      "ERR_INVALID_ARG_TYPE"
    );
  }
  function createTimeoutRangeError(value) {
    const error = new RangeError(
      `The value of "timeout" is out of range. It must be a non-negative finite number. Received ${String(value)}`
    );
    error.code = "ERR_OUT_OF_RANGE";
    return error;
  }
  function createListenArgValueError(message) {
    return createTypeErrorWithCode(message, "ERR_INVALID_ARG_VALUE");
  }
  function createSocketBadPortError(value) {
    const error = new RangeError(
      `options.port should be >= 0 and < 65536. Received ${formatReceivedType(value)}.`
    );
    error.code = "ERR_SOCKET_BAD_PORT";
    return error;
  }
  function isValidTcpPort(value) {
    return Number.isInteger(value) && value >= 0 && value < 65536;
  }
  function isDecimalIntegerString(value) {
    return /^[0-9]+$/.test(value);
  }
  function normalizeListenPortValue(value) {
    if (value === void 0 || value === null) {
      return 0;
    }
    if (typeof value === "string" && value.length > 0) {
      const parsed = Number(value);
      if (isValidTcpPort(parsed)) {
        return parsed;
      }
      throw createSocketBadPortError(value);
    }
    if (typeof value === "number") {
      if (isValidTcpPort(value)) {
        return value;
      }
      throw createSocketBadPortError(value);
    }
    throw createListenArgValueError(
      `The argument 'options' is invalid. Received ${String(value)}`
    );
  }
  function normalizeListenArgs(portOrOptions, hostOrCallback, backlogOrCallback, callback) {
    const defaultOptions = {
      port: 0,
      host: "127.0.0.1",
      backlog: 511,
      readableAll: false,
      writableAll: false
    };
    if (typeof portOrOptions === "function") {
      return {
        ...defaultOptions,
        callback: portOrOptions
      };
    }
    if (portOrOptions !== null && typeof portOrOptions === "object") {
      const options = portOrOptions;
      const hasPort = Object.prototype.hasOwnProperty.call(options, "port");
      const hasPath = Object.prototype.hasOwnProperty.call(options, "path");
      if (!hasPort && !hasPath) {
        throw createListenArgValueError(
          `The argument 'options' must have the property "port" or "path". Received ${String(portOrOptions)}`
        );
      }
      if (hasPort && hasPath) {
        throw createListenArgValueError(
          `The argument 'options' is invalid. Received ${String(portOrOptions)}`
        );
      }
      if (hasPort && options.port !== void 0 && options.port !== null && typeof options.port !== "number" && typeof options.port !== "string") {
        throw createListenArgValueError(
          `The argument 'options' is invalid. Received ${String(portOrOptions)}`
        );
      }
      if (hasPath) {
        if (typeof options.path !== "string" || options.path.length === 0) {
          throw createListenArgValueError(
            `The argument 'options' is invalid. Received ${String(portOrOptions)}`
          );
        }
        return {
          path: options.path,
          backlog: typeof options.backlog === "number" && Number.isFinite(options.backlog) ? options.backlog : defaultOptions.backlog,
          readableAll: options.readableAll === true,
          writableAll: options.writableAll === true,
          callback: typeof hostOrCallback === "function" ? hostOrCallback : typeof backlogOrCallback === "function" ? backlogOrCallback : callback
        };
      }
      return {
        port: normalizeListenPortValue(options.port),
        host: typeof options.host === "string" && options.host.length > 0 ? options.host : defaultOptions.host,
        backlog: typeof options.backlog === "number" && Number.isFinite(options.backlog) ? options.backlog : defaultOptions.backlog,
        readableAll: false,
        writableAll: false,
        callback: typeof hostOrCallback === "function" ? hostOrCallback : typeof backlogOrCallback === "function" ? backlogOrCallback : callback
      };
    }
    if (portOrOptions !== void 0 && portOrOptions !== null && typeof portOrOptions !== "number" && typeof portOrOptions !== "string") {
      throw createListenArgValueError(
        `The argument 'options' is invalid. Received ${String(portOrOptions)}`
      );
    }
    if (typeof portOrOptions === "string" && portOrOptions.length > 0 && !isDecimalIntegerString(portOrOptions)) {
      return {
        path: portOrOptions,
        backlog: defaultOptions.backlog,
        readableAll: false,
        writableAll: false,
        callback: typeof hostOrCallback === "function" ? hostOrCallback : typeof backlogOrCallback === "function" ? backlogOrCallback : callback
      };
    }
    return {
      port: normalizeListenPortValue(portOrOptions),
      host: typeof hostOrCallback === "string" ? hostOrCallback : defaultOptions.host,
      backlog: typeof backlogOrCallback === "number" ? backlogOrCallback : defaultOptions.backlog,
      readableAll: false,
      writableAll: false,
      callback: typeof hostOrCallback === "function" ? hostOrCallback : typeof backlogOrCallback === "function" ? backlogOrCallback : callback
    };
  }
  function normalizeConnectArgs(portOrOptions, hostOrCallback, callback) {
    if (portOrOptions !== null && typeof portOrOptions === "object") {
      const normalizedPort = typeof portOrOptions.port === "string" ? Number(portOrOptions.port) : portOrOptions.port;
      return {
        host: typeof portOrOptions.host === "string" && portOrOptions.host.length > 0 ? portOrOptions.host : void 0,
        port: normalizedPort,
        path: typeof portOrOptions.path === "string" && portOrOptions.path.length > 0 ? portOrOptions.path : void 0,
        keepAlive: portOrOptions.keepAlive,
        keepAliveInitialDelay: portOrOptions.keepAliveInitialDelay,
        callback: typeof hostOrCallback === "function" ? hostOrCallback : callback
      };
    }
    if (typeof portOrOptions === "string" && !isDecimalIntegerString(portOrOptions)) {
      return {
        path: portOrOptions,
        callback: typeof hostOrCallback === "function" ? hostOrCallback : callback
      };
    }
    return {
      port: typeof portOrOptions === "number" ? portOrOptions : Number(portOrOptions),
      host: typeof hostOrCallback === "string" ? hostOrCallback : "127.0.0.1",
      callback: typeof hostOrCallback === "function" ? hostOrCallback : callback
    };
  }
  function isValidIPv4Segment(segment) {
    if (!/^[0-9]{1,3}$/.test(segment)) {
      return false;
    }
    if (segment.length > 1 && segment.startsWith("0")) {
      return false;
    }
    const value = Number(segment);
    return Number.isInteger(value) && value >= 0 && value <= 255;
  }
  function isIPv4String(input) {
    const segments = input.split(".");
    return segments.length === 4 && segments.every((segment) => isValidIPv4Segment(segment));
  }
  function isValidIPv6Zone(zone) {
    return zone.length > 0 && /^[0-9A-Za-z_.-]+$/.test(zone);
  }
  function countIPv6Parts(part) {
    if (part.length === 0) {
      return 0;
    }
    const segments = part.split(":");
    let count = 0;
    for (const segment of segments) {
      if (segment.length === 0) {
        return null;
      }
      if (segment.includes(".")) {
        if (segment !== segments[segments.length - 1] || !isIPv4String(segment)) {
          return null;
        }
        count += 2;
        continue;
      }
      if (!/^[0-9A-Fa-f]{1,4}$/.test(segment)) {
        return null;
      }
      count += 1;
    }
    return count;
  }
  function isIPv6String(input) {
    if (input.length === 0) {
      return false;
    }
    let address = input;
    const zoneIndex = address.indexOf("%");
    if (zoneIndex !== -1) {
      if (address.indexOf("%", zoneIndex + 1) !== -1) {
        return false;
      }
      const zone = address.slice(zoneIndex + 1);
      if (!isValidIPv6Zone(zone)) {
        return false;
      }
      address = address.slice(0, zoneIndex);
    }
    const doubleColonIndex = address.indexOf("::");
    if (doubleColonIndex !== -1) {
      if (address.indexOf("::", doubleColonIndex + 2) !== -1) {
        return false;
      }
      const [left, right] = address.split("::");
      if (left.includes(".")) {
        return false;
      }
      const leftCount = countIPv6Parts(left);
      const rightCount = countIPv6Parts(right);
      if (leftCount === null || rightCount === null) {
        return false;
      }
      return leftCount + rightCount < 8;
    }
    const count = countIPv6Parts(address);
    return count === 8;
  }
  function coerceIpInput(input) {
    if (input === null || input === void 0) {
      return "";
    }
    return String(input);
  }
  function classifyIpAddress(input) {
    const value = coerceIpInput(input);
    if (isIPv4String(value)) {
      return 4;
    }
    if (isIPv6String(value)) {
      return 6;
    }
    return 0;
  }
  function normalizeSocketTimeout(timeout) {
    if (typeof timeout !== "number") {
      throw createTimeoutArgTypeError("timeout", timeout);
    }
    if (!Number.isFinite(timeout) || timeout < 0) {
      throw createTimeoutRangeError(timeout);
    }
    return timeout;
  }
  function parseNetSocketInfo(data) {
    if (!data) {
      return null;
    }
    try {
      const parsed = JSON.parse(data);
      return parsed && typeof parsed === "object" ? parsed : null;
    } catch {
      return null;
    }
  }
  function normalizeNetSocketHandle(handle) {
    if (!handle) {
      throw new Error("net.connect bridge returned an empty socket handle");
    }
    if (typeof handle === "string") {
      return {
        socketId: handle
      };
    }
    if (typeof handle === "object" && typeof handle.socketId === "string") {
      return handle;
    }
    throw new Error("net.connect bridge returned an invalid socket handle");
  }
  function serializeTlsValue(value) {
    if (value === void 0 || value === null) {
      return void 0;
    }
    if (Array.isArray(value)) {
      const entries = value.map((entry) => serializeTlsValue(entry)).flatMap((entry) => Array.isArray(entry) ? entry : entry ? [entry] : []);
      return entries.length > 0 ? entries : void 0;
    }
    if (typeof value === "string") {
      return { kind: "string", data: value };
    }
    if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
      return { kind: "buffer", data: Buffer.from(value).toString("base64") };
    }
    return void 0;
  }
  function isTlsSecureContextWrapper(value) {
    return !!value && typeof value === "object" && "__secureExecTlsContext" in value;
  }
  function buildSerializedTlsOptions(options, extra) {
    const contextOptions = isTlsSecureContextWrapper(options?.secureContext) ? options.secureContext.__secureExecTlsContext : void 0;
    const serialized = {
      ...contextOptions ?? {},
      ...extra
    };
    const key = serializeTlsValue(options?.key);
    const cert = serializeTlsValue(options?.cert);
    const ca = serializeTlsValue(options?.ca);
    if (key !== void 0) serialized.key = key;
    if (cert !== void 0) serialized.cert = cert;
    if (ca !== void 0) serialized.ca = ca;
    if (typeof options?.passphrase === "string") serialized.passphrase = options.passphrase;
    if (typeof options?.ciphers === "string") serialized.ciphers = options.ciphers;
    if (Buffer.isBuffer(options?.session) || options?.session instanceof Uint8Array) {
      serialized.session = Buffer.from(options.session).toString("base64");
    }
    if (Array.isArray(options?.ALPNProtocols)) {
      const protocols = options.ALPNProtocols.filter((value) => typeof value === "string");
      if (protocols.length > 0) {
        serialized.ALPNProtocols = protocols;
      }
    }
    if (typeof options?.minVersion === "string") serialized.minVersion = options.minVersion;
    if (typeof options?.maxVersion === "string") serialized.maxVersion = options.maxVersion;
    if (typeof options?.servername === "string") serialized.servername = options.servername;
    if (typeof options?.rejectUnauthorized === "boolean") {
      serialized.rejectUnauthorized = options.rejectUnauthorized;
    }
    if (typeof options?.requestCert === "boolean") {
      serialized.requestCert = options.requestCert;
    }
    return serialized;
  }
  function parseTlsState(payload) {
    if (!payload) {
      return null;
    }
    try {
      return JSON.parse(payload);
    } catch {
      return null;
    }
  }
  function parseTlsClientHello(payload) {
    if (!payload) {
      return null;
    }
    try {
      return JSON.parse(payload);
    } catch {
      return null;
    }
  }
  function createBridgedTlsError(payload) {
    if (!payload) {
      return new Error("socket error");
    }
    try {
      const parsed = JSON.parse(payload);
      const error = new Error(parsed.message);
      if (parsed.name) error.name = parsed.name;
      if (parsed.code) {
        error.code = parsed.code;
      }
      if (parsed.stack) error.stack = parsed.stack;
      return error;
    } catch {
      return new Error(payload);
    }
  }
  function deserializeTlsBridgeValue(value, refs = /* @__PURE__ */ new Map()) {
    if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
      return value;
    }
    if (value.type === "undefined") {
      return void 0;
    }
    if (value.type === "buffer") {
      return Buffer.from(value.data, "base64");
    }
    if (value.type === "array") {
      return value.value.map((entry) => deserializeTlsBridgeValue(entry, refs));
    }
    if (value.type === "ref") {
      return refs.get(value.id);
    }
    const target = {};
    refs.set(value.id, target);
    for (const [key, entry] of Object.entries(value.value)) {
      target[key] = deserializeTlsBridgeValue(entry, refs);
    }
    return target;
  }
  function queryTlsSocket(socketId, query, detailed) {
    if (typeof _netSocketTlsQueryRaw === "undefined") {
      return void 0;
    }
    const payload = _netSocketTlsQueryRaw.applySync(
      void 0,
      detailed === void 0 ? [socketId, query] : [socketId, query, detailed]
    );
    return deserializeTlsBridgeValue(JSON.parse(payload));
  }
  function createConnectedSocketHandle(socketId) {
    return {
      socketId,
      setNoDelay(enable) {
        _netSocketSetNoDelayRaw?.applySync(void 0, [socketId, enable !== false]);
        return this;
      },
      setKeepAlive(enable, initialDelay) {
        _netSocketSetKeepAliveRaw?.applySync(void 0, [
          socketId,
          enable !== false,
          normalizeKeepAliveDelay(initialDelay)
        ]);
        return this;
      },
      ref() {
        return this;
      },
      unref() {
        return this;
      }
    };
  }
  function createAcceptedClientHandle(socketId, info) {
    return {
      socketId,
      info,
      setNoDelay(enable) {
        _netSocketSetNoDelayRaw?.applySync(void 0, [socketId, enable !== false]);
        return this;
      },
      setKeepAlive(enable, initialDelay) {
        _netSocketSetKeepAliveRaw?.applySync(void 0, [
          socketId,
          enable !== false,
          normalizeKeepAliveDelay(initialDelay)
        ]);
        return this;
      },
      ref() {
        return this;
      },
      unref() {
        return this;
      }
    };
  }
  var NET_BRIDGE_TIMEOUT_SENTINEL = "__secure_exec_net_timeout__";
  var NET_BRIDGE_POLL_DELAY_MS = 10;
  function netSocketDispatch(socketId, event, data) {
    if (socketId === 0 && event.startsWith("http2:")) {
      debugBridgeNetwork("http2 dispatch via netSocket", event);
      try {
        const payload = data ? JSON.parse(data) : {};
        http2Dispatch(
          event.slice("http2:".length),
          Number(payload.id ?? 0),
          payload.data,
          payload.extra,
          payload.extraNumber,
          payload.extraHeaders,
          payload.flags
        );
      } catch {
      }
      return;
    }
    const socket = getRegisteredNetSocket(socketId);
    if (!socket) return;
    switch (event) {
      case "connect": {
        socket._applySocketInfo(parseNetSocketInfo(data));
        socket._connected = true;
        socket.connecting = false;
        socket._touchTimeout();
        socket._emitNet("connect");
        socket._emitNet("ready");
        break;
      }
      case "secureConnect":
      case "secure": {
        const state = parseTlsState(data);
        socket.encrypted = true;
        if (state) {
          socket.authorized = state.authorized === true;
          socket.authorizationError = state.authorizationError;
          socket.alpnProtocol = state.alpnProtocol ?? false;
          socket.servername = state.servername ?? socket.servername;
          socket._tlsProtocol = state.protocol ?? null;
          socket._tlsSessionReused = state.sessionReused === true;
          socket._tlsCipher = state.cipher ?? null;
        }
        socket._emitNet(event);
        break;
      }
      case "data": {
        const buf = typeof Buffer !== "undefined" ? Buffer.from(data, "base64") : new Uint8Array(0);
        socket._touchTimeout();
        socket._emitNet("data", buf);
        break;
      }
      case "end":
        socket._emitNet("end");
        break;
      case "session": {
        const session = typeof Buffer !== "undefined" ? Buffer.from(data ?? "", "base64") : new Uint8Array(0);
        socket._tlsSession = Buffer.from(session);
        socket._emitNet("session", session);
        break;
      }
      case "error":
        if (data) {
          try {
            const parsed = JSON.parse(data);
            socket.authorized = parsed.authorized === true;
            socket.authorizationError = parsed.authorizationError;
          } catch {
          }
        }
        socket._emitNet("error", createBridgedTlsError(data));
        break;
      case "close":
        unregisterNetSocket(socketId);
        socket._connected = false;
        socket.connecting = false;
        socket._clearTimeoutTimer();
        socket._emitNet("close");
        break;
    }
  }
  exposeCustomGlobal("_netSocketDispatch", netSocketDispatch);
  var NetSocket = class _NetSocket {
    _listeners = {};
    _onceListeners = {};
    _socketId = 0;
    _loopbackServer = null;
    _loopbackBuffer = Buffer.alloc(0);
    _loopbackDispatchRunning = false;
    _loopbackReadableEnded = false;
    _loopbackEventQueue = Promise.resolve();
    _encoding;
    _noDelayState = false;
    _keepAliveState = false;
    _keepAliveDelaySeconds = 0;
    _refed = true;
    _bridgeReadLoopRunning = false;
    _bridgeReadPollTimer = null;
    _timeoutMs = 0;
    _timeoutTimer = null;
    _tlsUpgrading = false;
    _connected = false;
    connecting = false;
    destroyed = false;
    writable = true;
    readable = true;
    readyState = "open";
    readableLength = 0;
    writableLength = 0;
    remoteAddress;
    remotePort;
    remoteFamily;
    localAddress = "0.0.0.0";
    localPort = 0;
    localFamily = "IPv4";
    localPath;
    remotePath;
    bytesRead = 0;
    bytesWritten = 0;
    bufferSize = 0;
    pending = true;
    allowHalfOpen = false;
    encrypted = false;
    authorized = false;
    authorizationError;
    servername;
    alpnProtocol = false;
    writableHighWaterMark = 16 * 1024;
    server;
    _tlsCipher = null;
    _tlsProtocol = null;
    _tlsSession = null;
    _tlsSessionReused = false;
    // Readable stream state stub for library compatibility
    _readableState = { endEmitted: false };
    _readQueue = [];
    _handle = null;
    constructor(options) {
      if (options?.allowHalfOpen) this.allowHalfOpen = true;
      if (options?.handle) this._handle = options.handle;
    }
    connect(portOrOptions, hostOrCallback, callback) {
      if (typeof _netSocketConnectRaw === "undefined") {
        throw new Error("net.Socket is not supported in sandbox (bridge not available)");
      }
      const {
        host = "127.0.0.1",
        port = 0,
        path,
        keepAlive,
        keepAliveInitialDelay,
        callback: cb
      } = normalizeConnectArgs(portOrOptions, hostOrCallback, callback);
      if (cb) this.once("connect", cb);
      this.connecting = true;
      this.remoteAddress = path ?? host;
      this.remotePort = path ? void 0 : port;
      this.remotePath = path;
      this.pending = false;
      const loopbackServer = !path && isLoopbackRequestHost(host) ? findLoopbackHttpServerByPort(port) : null;
      if (loopbackServer) {
        this._loopbackServer = loopbackServer;
        this._connected = true;
        this.connecting = false;
        queueMicrotask(() => {
          this._touchTimeout();
          this._emitNet("connect");
          this._emitNet("ready");
        });
        return this;
      }
      const handle = normalizeNetSocketHandle(_netSocketConnectRaw.applySync(
        void 0,
        [path ? { path } : { host, port }]
      ));
      debugBridgeNetwork("socket connect", handle.socketId, host, port, path ?? null);
      this._socketId = handle.socketId;
      this._handle = createConnectedSocketHandle(this._socketId);
      this._applySocketInfo(handle);
      registerNetSocket(this._socketId, this);
      void this._waitForConnect();
      if (keepAlive) {
        this.once("connect", () => {
          this.setKeepAlive(true, keepAliveInitialDelay);
        });
      }
      return this;
    }
    write(data, encodingOrCallback, callback) {
      let buf;
      if (Buffer.isBuffer(data)) {
        buf = data;
      } else if (typeof data === "string") {
        const enc = typeof encodingOrCallback === "string" ? encodingOrCallback : "utf-8";
        buf = Buffer.from(data, enc);
      } else {
        buf = Buffer.from(data);
      }
      if (this._loopbackServer) {
        debugBridgeNetwork("socket write loopback", this._socketId, buf.length);
        this.bytesWritten += buf.length;
        this._loopbackBuffer = Buffer.concat([this._loopbackBuffer, buf]);
        this._touchTimeout();
        this._dispatchLoopbackHttpRequest();
        const cb2 = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
        if (cb2) cb2();
        return true;
      }
      if (typeof _netSocketWriteRaw === "undefined") return false;
      if (this.destroyed || !this._socketId) return false;
      const base64 = buf.toString("base64");
      debugBridgeNetwork("socket write", this._socketId, buf.length, base64.slice(0, 64));
      this.bytesWritten += buf.length;
      _netSocketWriteRaw.applySync(void 0, [this._socketId, base64]);
      this._touchTimeout();
      const cb = typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
      if (cb) cb();
      return true;
    }
    end(dataOrCallback, encodingOrCallback, callback) {
      if (typeof dataOrCallback === "function") {
        this.once("finish", dataOrCallback);
      } else if (dataOrCallback != null) {
        this.write(dataOrCallback, encodingOrCallback, callback);
      }
      if (this._loopbackServer) {
        if (!this._loopbackReadableEnded) {
          queueMicrotask(() => {
            this._closeLoopbackReadable();
          });
        }
        return this;
      }
      if (typeof _netSocketEndRaw !== "undefined" && this._socketId && !this.destroyed) {
        debugBridgeNetwork("socket end", this._socketId);
        _netSocketEndRaw.applySync(void 0, [this._socketId]);
        this._touchTimeout();
      }
      return this;
    }
    destroy(error) {
      if (this.destroyed) return this;
      debugBridgeNetwork("socket destroy", this._socketId, error?.message ?? null);
      this.destroyed = true;
      this.writable = false;
      this.readable = false;
      this._clearTimeoutTimer();
      if (this._bridgeReadPollTimer) {
        clearTimeout(this._bridgeReadPollTimer);
        this._bridgeReadPollTimer = null;
      }
      if (this._loopbackServer) {
        this._loopbackServer = null;
        if (error) {
          this._emitNet("error", error);
        }
        this._emitNet("close");
        return this;
      }
      if (typeof _netSocketDestroyRaw !== "undefined" && this._socketId) {
        _netSocketDestroyRaw.applySync(void 0, [this._socketId]);
        unregisterNetSocket(this._socketId);
      }
      if (error) {
        this._emitNet("error", error);
      }
      this._emitNet("close");
      return this;
    }
    _applySocketInfo(info) {
      if (!info) {
        return;
      }
      this.localAddress = info.localAddress;
      this.localPort = info.localPort;
      this.localFamily = info.localFamily;
      this.localPath = info.localPath;
      this.remoteAddress = info.remoteAddress ?? this.remoteAddress;
      this.remotePort = info.remotePort ?? this.remotePort;
      this.remoteFamily = info.remoteFamily ?? this.remoteFamily;
      this.remotePath = info.remotePath ?? this.remotePath;
    }
    _applyAcceptedKeepAlive(initialDelay) {
      this._keepAliveState = true;
      this._keepAliveDelaySeconds = normalizeKeepAliveDelay(initialDelay);
    }
    static fromAcceptedHandle(handle, options) {
      const socket = new _NetSocket({ allowHalfOpen: options?.allowHalfOpen });
      socket._socketId = handle.socketId;
      socket._handle = createConnectedSocketHandle(handle.socketId);
      socket._applySocketInfo(handle.info);
      socket._connected = true;
      socket.connecting = false;
      socket.pending = false;
      registerNetSocket(handle.socketId, socket);
      queueMicrotask(() => {
        if (!socket.destroyed && !socket._tlsUpgrading) {
          void socket._pumpBridgeReads();
        }
      });
      return socket;
    }
    setKeepAlive(enable, initialDelay) {
      const nextEnable = isTruthySocketOption(enable);
      const nextDelaySeconds = normalizeKeepAliveDelay(initialDelay);
      if (nextEnable === this._keepAliveState && (!nextEnable || nextDelaySeconds === this._keepAliveDelaySeconds)) {
        return this;
      }
      this._keepAliveState = nextEnable;
      this._keepAliveDelaySeconds = nextEnable ? nextDelaySeconds : 0;
      debugBridgeNetwork("socket setKeepAlive", this._socketId, nextEnable, nextDelaySeconds);
      this._handle?.setKeepAlive?.(nextEnable, nextDelaySeconds);
      return this;
    }
    setNoDelay(noDelay) {
      const nextState = isTruthySocketOption(noDelay);
      if (nextState === this._noDelayState) {
        return this;
      }
      this._noDelayState = nextState;
      debugBridgeNetwork("socket setNoDelay", this._socketId, nextState);
      this._handle?.setNoDelay?.(nextState);
      return this;
    }
    setTimeout(timeout, callback) {
      const nextTimeout = normalizeSocketTimeout(timeout);
      if (callback !== void 0 && typeof callback !== "function") {
        throw createFunctionArgTypeError("callback", callback);
      }
      if (callback) {
        this.once("timeout", callback);
      }
      this._timeoutMs = nextTimeout;
      if (nextTimeout === 0) {
        this._clearTimeoutTimer();
        return this;
      }
      this._touchTimeout();
      return this;
    }
    ref() {
      this._refed = true;
      this._handle?.ref?.();
      if (this._timeoutTimer && typeof this._timeoutTimer.ref === "function") {
        this._timeoutTimer.ref();
      }
      if (!this.destroyed && this._connected && !this._loopbackServer && !this._bridgeReadLoopRunning) {
        void this._pumpBridgeReads();
      }
      return this;
    }
    unref() {
      this._refed = false;
      this._handle?.unref?.();
      if (this._timeoutTimer && typeof this._timeoutTimer.unref === "function") {
        this._timeoutTimer.unref();
      }
      if (this._bridgeReadPollTimer) {
        clearTimeout(this._bridgeReadPollTimer);
        this._bridgeReadPollTimer = null;
      }
      return this;
    }
    pause() {
      return this;
    }
    resume() {
      return this;
    }
    read(size) {
      if (this._readQueue.length === 0) {
        return null;
      }
      if (size == null || size <= 0) {
        const chunk = this._readQueue.shift() ?? null;
        if (chunk) {
          this.readableLength = Math.max(0, this.readableLength - chunk.length);
        }
        return chunk;
      }
      const head = this._readQueue[0];
      if (!head) {
        return null;
      }
      if (head.length <= size) {
        this._readQueue.shift();
        this.readableLength = Math.max(0, this.readableLength - head.length);
        return head;
      }
      const chunk = head.subarray(0, size);
      this._readQueue[0] = head.subarray(size);
      this.readableLength = Math.max(0, this.readableLength - chunk.length);
      return chunk;
    }
    unshift(chunk) {
      const payload = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      if (payload.length === 0) {
        return this;
      }
      this._readQueue.unshift(payload);
      this.readableLength += payload.length;
      return this;
    }
    cork() {
    }
    uncork() {
    }
    address() {
      return { port: this.localPort, family: this.localFamily, address: this.localAddress };
    }
    getCipher() {
      return queryTlsSocket(this._socketId, "getCipher") ?? this._tlsCipher;
    }
    getSession() {
      const session = queryTlsSocket(this._socketId, "getSession");
      if (Buffer.isBuffer(session)) {
        this._tlsSession = Buffer.from(session);
        return Buffer.from(session);
      }
      return this._tlsSession ? Buffer.from(this._tlsSession) : null;
    }
    isSessionReused() {
      const reused = queryTlsSocket(this._socketId, "isSessionReused");
      return typeof reused === "boolean" ? reused : this._tlsSessionReused;
    }
    getPeerCertificate(detailed) {
      const cert = queryTlsSocket(this._socketId, "getPeerCertificate", detailed === true);
      return cert && typeof cert === "object" ? cert : {};
    }
    getCertificate() {
      const cert = queryTlsSocket(this._socketId, "getCertificate");
      return cert && typeof cert === "object" ? cert : {};
    }
    getProtocol() {
      const protocol = queryTlsSocket(this._socketId, "getProtocol");
      return typeof protocol === "string" ? protocol : this._tlsProtocol;
    }
    setEncoding(encoding) {
      this._encoding = encoding;
      return this;
    }
    pipe(destination) {
      return destination;
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    addListener(event, listener) {
      return this.on(event, listener);
    }
    once(event, listener) {
      if (!this._onceListeners[event]) this._onceListeners[event] = [];
      this._onceListeners[event].push(listener);
      return this;
    }
    removeListener(event, listener) {
      const listeners = this._listeners[event];
      if (listeners) {
        const idx = listeners.indexOf(listener);
        if (idx >= 0) listeners.splice(idx, 1);
      }
      const onceListeners = this._onceListeners[event];
      if (onceListeners) {
        const idx = onceListeners.indexOf(listener);
        if (idx >= 0) onceListeners.splice(idx, 1);
      }
      return this;
    }
    off(event, listener) {
      return this.removeListener(event, listener);
    }
    removeAllListeners(event) {
      if (event) {
        delete this._listeners[event];
        delete this._onceListeners[event];
      } else {
        this._listeners = {};
        this._onceListeners = {};
      }
      return this;
    }
    listeners(event) {
      return [...this._listeners[event] ?? [], ...this._onceListeners[event] ?? []];
    }
    listenerCount(event) {
      return (this._listeners[event]?.length ?? 0) + (this._onceListeners[event]?.length ?? 0);
    }
    setMaxListeners(_n) {
      return this;
    }
    getMaxListeners() {
      return 10;
    }
    prependListener(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].unshift(listener);
      return this;
    }
    prependOnceListener(event, listener) {
      if (!this._onceListeners[event]) this._onceListeners[event] = [];
      this._onceListeners[event].unshift(listener);
      return this;
    }
    eventNames() {
      return [.../* @__PURE__ */ new Set([...Object.keys(this._listeners), ...Object.keys(this._onceListeners)])];
    }
    rawListeners(event) {
      return this.listeners(event);
    }
    emit(event, ...args) {
      return this._emitNet(event, ...args);
    }
    _emitNet(event, ...args) {
      if (event === "data" && this._encoding && args[0] && Buffer.isBuffer(args[0])) {
        args[0] = args[0].toString(this._encoding);
      }
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
        const fns = [...onceListeners];
        this._onceListeners[event] = [];
        for (const fn of fns) {
          fn.call(this, ...args);
          handled = true;
        }
      }
      return handled;
    }
    _queueReadablePayload(payload) {
      if (!payload || payload.length === 0) {
        return;
      }
      this._readQueue.push(payload);
      this.readableLength += payload.length;
      this._emitNet("readable");
      if (this.listenerCount("data") > 0) {
        const chunk = this.read();
        if (chunk !== null) {
          this._emitNet("data", chunk);
        }
      }
    }
    async _waitForConnect() {
      if (typeof _netSocketWaitConnectRaw === "undefined" || this._socketId === 0) {
        return;
      }
      try {
        const infoJson = await _netSocketWaitConnectRaw.apply(
          void 0,
          [this._socketId],
          { result: { promise: true } }
        );
        if (this.destroyed) {
          return;
        }
        this._applySocketInfo(parseNetSocketInfo(infoJson));
        this._connected = true;
        this.connecting = false;
        debugBridgeNetwork("socket connected", this._socketId, this.localAddress, this.localPort, this.remoteAddress, this.remotePort);
        this._touchTimeout();
        debugBridgeNetwork("socket emit connect", this._socketId, this.listenerCount("connect"));
        this._emitNet("connect");
        debugBridgeNetwork("socket emit ready", this._socketId, this.listenerCount("ready"));
        this._emitNet("ready");
        if (!this._tlsUpgrading) {
          await this._pumpBridgeReads();
        }
      } catch (error) {
        if (this.destroyed) {
          return;
        }
        const err = error instanceof Error ? error : new Error(String(error));
        debugBridgeNetwork("socket connect error", this._socketId, err.message, err.stack ?? null);
        this._emitNet("error", err);
        this.destroy();
      }
    }
    async _pumpBridgeReads() {
      if (this._bridgeReadLoopRunning || typeof _netSocketReadRaw === "undefined" || this._socketId === 0) {
        return;
      }
      this._bridgeReadLoopRunning = true;
      try {
        while (!this.destroyed) {
          const chunkBase64 = _netSocketReadRaw.applySync(void 0, [this._socketId]);
          if (this.destroyed) {
            return;
          }
          if (chunkBase64 === NET_BRIDGE_TIMEOUT_SENTINEL) {
            if (!this._refed) {
              return;
            }
            this._bridgeReadPollTimer = setTimeout(() => {
              this._bridgeReadPollTimer = null;
              void this._pumpBridgeReads();
            }, NET_BRIDGE_POLL_DELAY_MS);
            return;
          }
          if (chunkBase64 === null) {
            debugBridgeNetwork("socket remote end", this._socketId);
            this.readable = false;
            this._readableState.endEmitted = true;
            this._emitNet("end");
            if (!this.destroyed) {
              unregisterNetSocket(this._socketId);
              this._emitNet("close");
            }
            return;
          }
          const payload = Buffer.from(chunkBase64, "base64");
          debugBridgeNetwork("socket data", this._socketId, payload.length);
          this.bytesRead += payload.length;
          this._touchTimeout();
          this._queueReadablePayload(payload);
        }
      } finally {
        this._bridgeReadLoopRunning = false;
      }
    }
    _dispatchLoopbackHttpRequest() {
      if (!this._loopbackServer || this._loopbackDispatchRunning || this.destroyed) {
        return;
      }
      this._loopbackDispatchRunning = true;
      void this._processLoopbackHttpRequests().finally(() => {
        this._loopbackDispatchRunning = false;
      });
    }
    async _processLoopbackHttpRequests() {
      let closeAfterDrain = false;
      while (this._loopbackServer && !this.destroyed) {
        const parsed = parseLoopbackRequestBuffer(this._loopbackBuffer, this._loopbackServer);
        if (parsed.kind === "incomplete") {
          if (closeAfterDrain) {
            this._closeLoopbackReadable();
          }
          return;
        }
        if (parsed.kind === "bad-request") {
          this._pushLoopbackData(createBadRequestResponseBuffer());
          if (parsed.closeConnection) {
            this._closeLoopbackReadable();
          }
          this._loopbackBuffer = Buffer.alloc(0);
          return;
        }
        this._loopbackBuffer = this._loopbackBuffer.subarray(parsed.bytesConsumed);
        if (parsed.upgradeHead) {
          this._dispatchLoopbackUpgrade(parsed.request, parsed.upgradeHead);
          return;
        }
        const {
          responseJson
        } = await dispatchLoopbackServerRequest(this._loopbackServer, parsed.request);
        const response = JSON.parse(responseJson);
        const serialized = serializeLoopbackResponse(response, parsed.request, parsed.closeConnection);
        if (!closeAfterDrain && serialized.payload.length > 0) {
          this._pushLoopbackData(serialized.payload);
        }
        if (serialized.closeConnection) {
          closeAfterDrain = true;
          if (this._loopbackBuffer.length === 0) {
            this._closeLoopbackReadable();
            return;
          }
        }
      }
    }
    _pushLoopbackData(data) {
      if (data.length === 0 || this._loopbackReadableEnded) {
        return;
      }
      const payload = Buffer.from(data);
      this._queueLoopbackEvent(() => {
        if (this.destroyed) {
          return;
        }
        this.bytesRead += payload.length;
        this._touchTimeout();
        this._queueReadablePayload(payload);
      });
    }
    _closeLoopbackReadable() {
      if (this._loopbackReadableEnded) {
        return;
      }
      this._loopbackReadableEnded = true;
      this.readable = false;
      this.writable = false;
      this._readableState.endEmitted = true;
      this._clearTimeoutTimer();
      this._queueLoopbackEvent(() => {
        this._emitNet("end");
        this._emitNet("close");
      });
    }
    _queueLoopbackEvent(callback) {
      this._loopbackEventQueue = this._loopbackEventQueue.then(
        () => new Promise((resolve) => {
          queueMicrotask(() => {
            try {
              callback();
            } finally {
              resolve();
            }
          });
        })
      );
    }
    _dispatchLoopbackUpgrade(request, head) {
      if (!this._loopbackServer) {
        return;
      }
      try {
        this._loopbackServer._emit(
          "upgrade",
          new ServerIncomingMessage(request),
          new DirectTunnelSocket({
            host: this.remoteAddress,
            port: this.remotePort
          }),
          head
        );
      } catch (error) {
        const rethrow = error instanceof Error ? error : new Error(String(error));
        let handled = false;
        let exitCodeFromHandler = null;
        if (typeof process !== "undefined" && typeof process.emit === "function") {
          const processEmitter = process;
          try {
            handled = processEmitter.emit("uncaughtException", rethrow, "uncaughtException");
          } catch (emitError) {
            if (emitError && typeof emitError === "object" && emitError.name === "ProcessExitError") {
              handled = true;
              const exitCode = Number(emitError.code);
              exitCodeFromHandler = Number.isFinite(exitCode) ? exitCode : 0;
            } else {
              throw emitError;
            }
          }
        }
        if (handled) {
          if (exitCodeFromHandler !== null) {
            process.exitCode = exitCodeFromHandler;
          }
          this._loopbackServer?.close();
          this.destroy();
          return;
        }
        throw rethrow;
      }
    }
    // Upgrade this socket to TLS
    _upgradeTls(options) {
      if (typeof _netSocketUpgradeTlsRaw === "undefined") {
        throw new Error("tls.connect is not supported in sandbox (bridge not available)");
      }
      this._tlsUpgrading = true;
      _netSocketUpgradeTlsRaw.applySync(void 0, [this._socketId, JSON.stringify(options ?? {})]);
    }
    _touchTimeout() {
      if (this._timeoutMs === 0 || this.destroyed) {
        return;
      }
      this._clearTimeoutTimer();
      this._timeoutTimer = setTimeout(() => {
        this._timeoutTimer = null;
        if (this.destroyed) {
          return;
        }
        this._emitNet("timeout");
      }, this._timeoutMs);
      if (!this._refed && typeof this._timeoutTimer.unref === "function") {
        this._timeoutTimer.unref();
      }
    }
    _clearTimeoutTimer() {
      if (this._timeoutTimer) {
        clearTimeout(this._timeoutTimer);
        this._timeoutTimer = null;
      }
    }
  };
  function netConnect(portOrOptions, hostOrCallback, callback) {
    const socket = new NetSocket();
    socket.connect(portOrOptions, hostOrCallback, callback);
    return socket;
  }
  var NetServer = class {
    _listeners = {};
    _onceListeners = {};
    _serverId = 0;
    _address = null;
    _acceptLoopActive = false;
    _acceptLoopRunning = false;
    _acceptPollTimer = null;
    _handleRefId = null;
    _connections = /* @__PURE__ */ new Set();
    _refed = true;
    listening = false;
    keepAlive = false;
    keepAliveInitialDelay = 0;
    allowHalfOpen = false;
    maxConnections;
    _handle;
    constructor(optionsOrListener, maybeListener) {
      if (typeof optionsOrListener === "function") {
        this.on("connection", optionsOrListener);
      } else {
        this.allowHalfOpen = optionsOrListener?.allowHalfOpen === true;
        this.keepAlive = optionsOrListener?.keepAlive === true;
        this.keepAliveInitialDelay = optionsOrListener?.keepAliveInitialDelay ?? 0;
        if (maybeListener) {
          this.on("connection", maybeListener);
        }
      }
      this._handle = {
        onconnection: (err, clientHandle) => {
          if (err) {
            this._emit("error", err);
            return;
          }
          if (!clientHandle) {
            return;
          }
          if (typeof this.maxConnections === "number" && this.maxConnections >= 0 && this._connections.size >= this.maxConnections) {
            this._emit("drop", {
              localAddress: clientHandle.info.localAddress,
              localPort: clientHandle.info.localPort,
              localFamily: clientHandle.info.localFamily,
              remoteAddress: clientHandle.info.remoteAddress,
              remotePort: clientHandle.info.remotePort,
              remoteFamily: clientHandle.info.remoteFamily
            });
            _netSocketDestroyRaw?.applySync(void 0, [clientHandle.socketId]);
            return;
          }
          if (this.keepAlive) {
            clientHandle.setKeepAlive?.(true, this.keepAliveInitialDelay);
          }
          const socket = NetSocket.fromAcceptedHandle(clientHandle, {
            allowHalfOpen: this.allowHalfOpen
          });
          socket.server = this;
          this._connections.add(socket);
          socket.once("close", () => {
            this._connections.delete(socket);
          });
          if (this.keepAlive) {
            socket._applyAcceptedKeepAlive(this.keepAliveInitialDelay);
          }
          this._emit("connection", socket);
        }
      };
    }
    listen(portOrOptions, hostOrCallback, backlogOrCallback, callback) {
      if (typeof _netServerListenRaw === "undefined" || typeof _netServerAcceptRaw === "undefined") {
        throw new Error("net.createServer is not supported in sandbox");
      }
      const { port, host, path, backlog, readableAll, writableAll, callback: cb } = normalizeListenArgs(
        portOrOptions,
        hostOrCallback,
        backlogOrCallback,
        callback
      );
      if (cb) {
        this.once("listening", cb);
      }
      try {
        const resultValue = _netServerListenRaw.applySyncPromise(
          void 0,
          [{ port, host, path, backlog, readableAll, writableAll }]
        );
        const result = typeof resultValue === "string" ? JSON.parse(resultValue) : resultValue;
        const address = result.address ?? result;
        this._serverId = result.serverId;
        this._address = address.localPath ? address.localPath : {
          address: address.localAddress,
          family: address.localFamily ?? address.family,
          port: address.localPort
        };
        this.listening = true;
        this._syncHandleRef();
        this._acceptLoopActive = true;
        queueMicrotask(() => {
          if (!this.listening || this._serverId === 0) {
            return;
          }
          this._emit("listening");
          void this._pumpAccepts();
        });
      } catch (error) {
        queueMicrotask(() => {
          this._emit("error", error);
        });
      }
      return this;
    }
    close(callback) {
      if (callback) {
        this.once("close", callback);
      }
      if (!this.listening || typeof _netServerCloseRaw === "undefined") {
        queueMicrotask(() => {
          this._emit("close");
        });
        return this;
      }
      this.listening = false;
      this._acceptLoopActive = false;
      if (this._acceptPollTimer) {
        clearTimeout(this._acceptPollTimer);
        this._acceptPollTimer = null;
      }
      this._syncHandleRef();
      const serverId = this._serverId;
      this._serverId = 0;
      void (async () => {
        try {
          await _netServerCloseRaw.apply(void 0, [serverId], {
            result: { promise: true }
          });
        } finally {
          this._address = null;
          this._emit("close");
        }
      })();
      return this;
    }
    address() {
      return this._address;
    }
    getConnections(callback) {
      if (typeof callback !== "function") {
        throw createFunctionArgTypeError("callback", callback);
      }
      queueMicrotask(() => {
        callback(null, this._connections.size);
      });
      return this;
    }
    ref() {
      this._refed = true;
      this._syncHandleRef();
      if (this.listening && this._acceptLoopActive && !this._acceptLoopRunning) {
        void this._pumpAccepts();
      }
      return this;
    }
    unref() {
      this._refed = false;
      if (this._acceptPollTimer) {
        clearTimeout(this._acceptPollTimer);
        this._acceptPollTimer = null;
      }
      this._syncHandleRef();
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
    _syncHandleRef() {
      if (!this.listening || this._serverId === 0 || !this._refed) {
        if (this._handleRefId && typeof _unregisterHandle === "function") {
          _unregisterHandle(this._handleRefId);
        }
        this._handleRefId = null;
        return;
      }
      const nextHandleId = `${NET_SERVER_HANDLE_PREFIX}${this._serverId}`;
      if (this._handleRefId === nextHandleId) {
        return;
      }
      if (this._handleRefId && typeof _unregisterHandle === "function") {
        _unregisterHandle(this._handleRefId);
      }
      this._handleRefId = nextHandleId;
      if (typeof _registerHandle === "function") {
        _registerHandle(this._handleRefId, "net server");
      }
    }
    async _pumpAccepts() {
      if (typeof _netServerAcceptRaw === "undefined" || this._acceptLoopRunning) {
        return;
      }
      this._acceptLoopRunning = true;
      try {
        while (this._acceptLoopActive && this._serverId !== 0) {
          const payload = _netServerAcceptRaw.applySync(void 0, [this._serverId]);
          if (payload === NET_BRIDGE_TIMEOUT_SENTINEL) {
            if (!this._refed) {
              return;
            }
            this._acceptPollTimer = setTimeout(() => {
              this._acceptPollTimer = null;
              void this._pumpAccepts();
            }, NET_BRIDGE_POLL_DELAY_MS);
            return;
          }
          if (!payload) {
            return;
          }
          try {
            const accepted = JSON.parse(payload);
            const clientHandle = createAcceptedClientHandle(accepted.socketId, accepted.info);
            this._handle.onconnection(null, clientHandle);
          } catch (error) {
            this._emit("error", error);
          }
        }
      } finally {
        this._acceptLoopRunning = false;
      }
    }
  };
  function NetServerCallable(optionsOrListener, maybeListener) {
    return new NetServer(optionsOrListener, maybeListener);
  }
  function findLoopbackHttpServerByPort(port) {
    for (const server of serverInstances.values()) {
      if (!server.listening) {
        continue;
      }
      const address = server.address();
      if (address && typeof address === "object" && address.port === port) {
        return server;
      }
    }
    return null;
  }
  var netModule = {
    Socket: NetSocket,
    Server: NetServerCallable,
    connect: netConnect,
    createConnection: netConnect,
    createServer(optionsOrListener, maybeListener) {
      return new NetServer(optionsOrListener, maybeListener);
    },
    isIP(input) {
      return classifyIpAddress(input);
    },
    isIPv4(input) {
      return classifyIpAddress(input) === 4;
    },
    isIPv6(input) {
      return classifyIpAddress(input) === 6;
    }
  };
  function createSecureContextWrapper(options) {
    return {
      __secureExecTlsContext: buildSerializedTlsOptions(options),
      context: {}
    };
  }
  function tlsConnect(portOrOptions, hostOrCallback, callback) {
    let socket;
    let options = {};
    let cb;
    if (typeof portOrOptions === "object") {
      options = { ...portOrOptions };
      cb = typeof hostOrCallback === "function" ? hostOrCallback : callback;
      if (portOrOptions.socket) {
        socket = portOrOptions.socket;
      } else {
        socket = new NetSocket();
        socket.connect({ host: portOrOptions.host ?? "127.0.0.1", port: portOrOptions.port });
      }
    } else {
      const host = typeof hostOrCallback === "string" ? hostOrCallback : "127.0.0.1";
      cb = typeof hostOrCallback === "function" ? hostOrCallback : callback;
      options = { host };
      socket = new NetSocket();
      socket.connect(portOrOptions, host);
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
        ...selectedContext?.__secureExecTlsContext ?? {},
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
  var tlsModule = {
    connect: tlsConnect,
    TLSSocket: NetSocket,
    // Alias — TLSSocket is just a NetSocket after upgrade
    Server: TLSServerCallable,
    createServer(optionsOrListener, maybeListener) {
      return new TLSServer(optionsOrListener, maybeListener);
    },
    createSecureContext(options) {
      return createSecureContextWrapper(options);
    },
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
    DEFAULT_MAX_VERSION: "TLSv1.3"
  };
  var DGRAM_HANDLE_PREFIX = "dgram-socket:";
  function createBadDgramSocketTypeError() {
    return createTypeErrorWithCode(
      "Bad socket type specified. Valid types are: udp4, udp6",
      "ERR_SOCKET_BAD_TYPE"
    );
  }
  function createDgramAlreadyBoundError() {
    const error = new Error("Socket is already bound");
    error.code = "ERR_SOCKET_ALREADY_BOUND";
    return error;
  }
  function createDgramAddressError() {
    return new Error("getsockname EBADF");
  }
  function createDgramArgTypeError(argumentName, expectedType, value) {
    return createTypeErrorWithCode(
      `The "${argumentName}" argument must be of type ${expectedType}. Received ${formatReceivedType(value)}`,
      "ERR_INVALID_ARG_TYPE"
    );
  }
  function createDgramMissingArgError(argumentName) {
    return createTypeErrorWithCode(
      `The "${argumentName}" argument must be specified`,
      "ERR_MISSING_ARGS"
    );
  }
  function createDgramNotRunningError() {
    return createErrorWithCode("Not running", "ERR_SOCKET_DGRAM_NOT_RUNNING");
  }
  function getDgramErrno(code) {
    switch (code) {
      case "EBADF":
        return -9;
      case "EINVAL":
        return -22;
      case "EADDRNOTAVAIL":
        return -99;
      case "ENOPROTOOPT":
        return -92;
    }
  }
  function createDgramSyscallError(syscall, code) {
    const error = new Error(`${syscall} ${code}`);
    error.code = code;
    error.errno = getDgramErrno(code);
    error.syscall = syscall;
    return error;
  }
  function createDgramTtlArgTypeError(value) {
    return createTypeErrorWithCode(
      `The "ttl" argument must be of type number. Received ${formatReceivedType(value)}`,
      "ERR_INVALID_ARG_TYPE"
    );
  }
  function createDgramBufferSizeTypeError() {
    return createTypeErrorWithCode(
      "Buffer size must be a positive integer",
      "ERR_SOCKET_BAD_BUFFER_SIZE"
    );
  }
  function createDgramBufferSizeSystemError(which, code) {
    const syscall = `uv_${which}_buffer_size`;
    const info = {
      errno: code === "EBADF" ? -9 : -22,
      code,
      message: code === "EBADF" ? "bad file descriptor" : "invalid argument",
      syscall
    };
    const error = new Error(
      `Could not get or set buffer size: ${syscall} returned ${code} (${info.message})`
    );
    error.name = "SystemError [ERR_SOCKET_BUFFER_SIZE]";
    error.code = "ERR_SOCKET_BUFFER_SIZE";
    error.info = info;
    let errno2 = info.errno;
    let syscallValue = syscall;
    Object.defineProperty(error, "errno", {
      enumerable: true,
      configurable: true,
      get() {
        return errno2;
      },
      set(value) {
        errno2 = value;
      }
    });
    Object.defineProperty(error, "syscall", {
      enumerable: true,
      configurable: true,
      get() {
        return syscallValue;
      },
      set(value) {
        syscallValue = value;
      }
    });
    return error;
  }
  function getPlatformDgramBufferSize(size) {
    if (size <= 0) {
      return size;
    }
    return process.platform === "linux" ? size * 2 : size;
  }
  function normalizeDgramTtlValue(value, syscall) {
    if (typeof value !== "number") {
      throw createDgramTtlArgTypeError(value);
    }
    if (!Number.isInteger(value) || value <= 0 || value >= 256) {
      throw createDgramSyscallError(syscall, "EINVAL");
    }
    return value;
  }
  function isIPv4MulticastAddress(address) {
    if (!isIPv4String(address)) {
      return false;
    }
    const first = Number(address.split(".")[0]);
    return first >= 224 && first <= 239;
  }
  function isIPv4UnicastAddress(address) {
    return isIPv4String(address) && !isIPv4MulticastAddress(address) && address !== "255.255.255.255";
  }
  function isIPv6MulticastAddress(address) {
    const zoneIndex = address.indexOf("%");
    const normalized = zoneIndex === -1 ? address : address.slice(0, zoneIndex);
    return isIPv6String(address) && normalized.toLowerCase().startsWith("ff");
  }
  function validateDgramMulticastAddress(type, syscall, address) {
    if (typeof address !== "string") {
      throw createDgramArgTypeError(
        syscall === "addSourceSpecificMembership" || syscall === "dropSourceSpecificMembership" ? "groupAddress" : "multicastAddress",
        "string",
        address
      );
    }
    const isValid = type === "udp6" ? isIPv6MulticastAddress(address) : isIPv4MulticastAddress(address);
    if (!isValid) {
      throw createDgramSyscallError(syscall, "EINVAL");
    }
    return address;
  }
  function validateDgramSourceAddress(type, syscall, address) {
    if (typeof address !== "string") {
      throw createDgramArgTypeError("sourceAddress", "string", address);
    }
    const isValid = type === "udp6" ? isIPv6String(address) && !isIPv6MulticastAddress(address) : isIPv4UnicastAddress(address);
    if (!isValid) {
      throw createDgramSyscallError(syscall, "EINVAL");
    }
    return address;
  }
  function normalizeDgramSocketType(value) {
    if (value === "udp4" || value === "udp6") {
      return value;
    }
    throw createBadDgramSocketTypeError();
  }
  function normalizeDgramSocketOptions(options) {
    if (typeof options === "string") {
      return { type: normalizeDgramSocketType(options) };
    }
    if (!options || typeof options !== "object" || Array.isArray(options)) {
      throw createBadDgramSocketTypeError();
    }
    const typedOptions = options;
    const result = {
      type: normalizeDgramSocketType(typedOptions.type)
    };
    if (typedOptions.recvBufferSize !== void 0) {
      if (typeof typedOptions.recvBufferSize !== "number") {
        throw createInvalidArgTypeError2(
          "options.recvBufferSize",
          "number",
          typedOptions.recvBufferSize
        );
      }
      result.recvBufferSize = typedOptions.recvBufferSize;
    }
    if (typedOptions.sendBufferSize !== void 0) {
      if (typeof typedOptions.sendBufferSize !== "number") {
        throw createInvalidArgTypeError2(
          "options.sendBufferSize",
          "number",
          typedOptions.sendBufferSize
        );
      }
      result.sendBufferSize = typedOptions.sendBufferSize;
    }
    return result;
  }
  function normalizeDgramAddressValue(address, type, defaultAddress) {
    if (address === void 0 || address === null || address === "") {
      return defaultAddress;
    }
    if (typeof address !== "string") {
      throw createDgramArgTypeError("address", "string", address);
    }
    if (address === "localhost") {
      return type === "udp6" ? "::1" : "127.0.0.1";
    }
    return address;
  }
  function normalizeDgramPortValue(port) {
    if (typeof port !== "number") {
      throw createDgramArgTypeError("port", "number", port);
    }
    if (!isValidTcpPort(port)) {
      throw createSocketBadPortError(port);
    }
    return port;
  }
  function createDgramMessageBuffer(value) {
    if (typeof value === "string") {
      return Buffer.from(value);
    }
    if (Buffer.isBuffer(value)) {
      return Buffer.from(value);
    }
    if (ArrayBuffer.isView(value)) {
      return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
    }
    throw createDgramArgTypeError("msg", "string or Buffer or Uint8Array or DataView", value);
  }
  function createDgramMessageListBuffer(value) {
    if (Array.isArray(value)) {
      return Buffer.concat(value.map((entry) => createDgramMessageBuffer(entry)));
    }
    return createDgramMessageBuffer(value);
  }
  function normalizeDgramBridgeResult(value) {
    if (typeof value !== "string") {
      return value;
    }
    try {
      return JSON.parse(value);
    } catch {
      return value;
    }
  }
  function decodeDgramBridgeBytes(value) {
    if (Buffer.isBuffer(value)) {
      return Buffer.from(value);
    }
    if (value instanceof Uint8Array) {
      return Buffer.from(value);
    }
    if (typeof value === "string") {
      return Buffer.from(value, "base64");
    }
    if (value && typeof value === "object") {
      if (value.__type === "Buffer" && typeof value.data === "string") {
        return Buffer.from(value.data, "base64");
      }
      if (value.__agentOsType === "bytes" && typeof value.base64 === "string") {
        return Buffer.from(value.base64, "base64");
      }
    }
    return Buffer.alloc(0);
  }
  function normalizeDgramBindArgs(args, type) {
    let port;
    let address;
    let callback;
    if (typeof args[0] === "function") {
      callback = args[0];
    } else if (args[0] && typeof args[0] === "object" && !Array.isArray(args[0])) {
      const options = args[0];
      port = options.port;
      address = options.address;
      callback = args[1];
    } else {
      port = args[0];
      if (typeof args[1] === "function") {
        callback = args[1];
      } else {
        address = args[1];
        callback = args[2];
      }
    }
    if (callback !== void 0 && typeof callback !== "function") {
      throw createFunctionArgTypeError("callback", callback);
    }
    return {
      port: port === void 0 ? 0 : normalizeDgramPortValue(port),
      address: normalizeDgramAddressValue(
        address,
        type,
        type === "udp6" ? "::" : "0.0.0.0"
      ),
      callback
    };
  }
  function normalizeDgramSendArgs(args, type) {
    if (args.length === 0) {
      throw createDgramArgTypeError("msg", "string or Buffer or Uint8Array or DataView", void 0);
    }
    const message = args[0];
    const hasOffsetLength = typeof args[1] === "number" && typeof args[2] === "number" && args.length >= 4;
    if (hasOffsetLength) {
      const source = createDgramMessageBuffer(message);
      const offset = args[1];
      const length = args[2];
      const callback2 = typeof args[4] === "function" ? args[4] : args[5];
      if (callback2 !== void 0 && typeof callback2 !== "function") {
        throw createFunctionArgTypeError("callback", callback2);
      }
      return {
        data: Buffer.from(source.subarray(offset, offset + length)),
        port: normalizeDgramPortValue(args[3]),
        address: normalizeDgramAddressValue(
          typeof args[4] === "function" ? void 0 : args[4],
          type,
          type === "udp6" ? "::1" : "127.0.0.1"
        ),
        callback: callback2
      };
    }
    const callback = typeof args[2] === "function" ? args[2] : args[3];
    if (callback !== void 0 && typeof callback !== "function") {
      throw createFunctionArgTypeError("callback", callback);
    }
    return {
      data: createDgramMessageListBuffer(message),
      port: normalizeDgramPortValue(args[1]),
      address: normalizeDgramAddressValue(
        typeof args[2] === "function" ? void 0 : args[2],
        type,
        type === "udp6" ? "::1" : "127.0.0.1"
      ),
      callback
    };
  }
  var DgramSocket = class {
    _type;
    _socketId;
    _listeners = {};
    _onceListeners = {};
    _bindPromise = null;
    _receiveLoopRunning = false;
    _receivePollTimer = null;
    _refed = true;
    _closed = false;
    _bound = false;
    _handleRefId = null;
    _recvBufferSize;
    _sendBufferSize;
    _memberships = /* @__PURE__ */ new Set();
    _multicastInterface;
    _broadcast = false;
    _multicastLoopback = 1;
    _multicastTtl = 1;
    _ttl = 64;
    constructor(optionsOrType, listener) {
      if (typeof _dgramSocketCreateRaw === "undefined") {
        throw new Error("dgram.createSocket is not supported in sandbox");
      }
      const options = normalizeDgramSocketOptions(optionsOrType);
      this._type = options.type;
      const result = normalizeDgramBridgeResult(
        _dgramSocketCreateRaw.applySync(void 0, [{ type: this._type }])
      );
      this._socketId = String(result?.socketId ?? result);
      if (listener) {
        this.on("message", listener);
      }
      if (options.recvBufferSize !== void 0) {
        this._setBufferSize("recv", options.recvBufferSize, false);
      }
      if (options.sendBufferSize !== void 0) {
        this._setBufferSize("send", options.sendBufferSize, false);
      }
    }
    bind(...args) {
      const { port, address, callback } = normalizeDgramBindArgs(args, this._type);
      void this._bindInternal(port, address, callback);
      return this;
    }
    send(...args) {
      const { data, port, address, callback } = normalizeDgramSendArgs(args, this._type);
      void this._sendInternal(data, port, address, callback);
    }
    sendto(...args) {
      this.send(...args);
    }
    address() {
      if (typeof _dgramSocketAddressRaw === "undefined") {
        throw createDgramAddressError();
      }
      try {
        return normalizeDgramBridgeResult(
          _dgramSocketAddressRaw.applySync(void 0, [this._socketId])
        );
      } catch {
        throw createDgramAddressError();
      }
    }
    close(callback) {
      if (callback !== void 0 && typeof callback !== "function") {
        throw createFunctionArgTypeError("callback", callback);
      }
      if (callback) {
        this.once("close", callback);
      }
      if (this._closed) {
        return this;
      }
      this._closed = true;
      this._bound = false;
      this._clearReceivePollTimer();
      this._syncHandleRef();
      if (typeof _dgramSocketCloseRaw === "undefined") {
        queueMicrotask(() => {
          this._emit("close");
        });
        return this;
      }
      try {
        _dgramSocketCloseRaw.applySyncPromise(void 0, [this._socketId]);
      } finally {
        queueMicrotask(() => {
          this._emit("close");
        });
      }
      return this;
    }
    ref() {
      this._refed = true;
      this._syncHandleRef();
      if (this._receivePollTimer && typeof this._receivePollTimer.ref === "function") {
        this._receivePollTimer.ref();
      }
      if (this._bound && !this._closed && !this._receiveLoopRunning) {
        void this._pumpMessages();
      }
      return this;
    }
    unref() {
      this._refed = false;
      this._syncHandleRef();
      if (this._receivePollTimer && typeof this._receivePollTimer.unref === "function") {
        this._receivePollTimer.unref();
      }
      return this;
    }
    setRecvBufferSize(size) {
      this._setBufferSize("recv", size);
    }
    setSendBufferSize(size) {
      this._setBufferSize("send", size);
    }
    getRecvBufferSize() {
      return this._getBufferSize("recv");
    }
    getSendBufferSize() {
      return this._getBufferSize("send");
    }
    setBroadcast(flag) {
      this._ensureBoundForSocketOption("setBroadcast");
      this._broadcast = Boolean(flag);
    }
    setTTL(ttl) {
      this._ensureBoundForSocketOption("setTTL");
      this._ttl = normalizeDgramTtlValue(ttl, "setTTL");
      return this._ttl;
    }
    setMulticastTTL(ttl) {
      this._ensureBoundForSocketOption("setMulticastTTL");
      this._multicastTtl = normalizeDgramTtlValue(ttl, "setMulticastTTL");
      return this._multicastTtl;
    }
    setMulticastLoopback(flag) {
      this._ensureBoundForSocketOption("setMulticastLoopback");
      this._multicastLoopback = Number(flag);
      return this._multicastLoopback;
    }
    addMembership(multicastAddress, multicastInterface) {
      if (multicastAddress === void 0) {
        throw createDgramMissingArgError("multicastAddress");
      }
      if (this._closed) {
        throw createDgramNotRunningError();
      }
      const groupAddress = validateDgramMulticastAddress(
        this._type,
        "addMembership",
        multicastAddress
      );
      if (multicastInterface !== void 0 && typeof multicastInterface !== "string") {
        throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
      }
      this._memberships.add(`${groupAddress}|${multicastInterface ?? ""}`);
    }
    dropMembership(multicastAddress, multicastInterface) {
      if (multicastAddress === void 0) {
        throw createDgramMissingArgError("multicastAddress");
      }
      if (this._closed) {
        throw createDgramNotRunningError();
      }
      const groupAddress = validateDgramMulticastAddress(
        this._type,
        "dropMembership",
        multicastAddress
      );
      if (multicastInterface !== void 0 && typeof multicastInterface !== "string") {
        throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
      }
      const membershipKey = `${groupAddress}|${multicastInterface ?? ""}`;
      if (!this._memberships.has(membershipKey)) {
        throw createDgramSyscallError("dropMembership", "EADDRNOTAVAIL");
      }
      this._memberships.delete(membershipKey);
    }
    addSourceSpecificMembership(sourceAddress, groupAddress, multicastInterface) {
      if (this._closed) {
        throw createDgramNotRunningError();
      }
      if (typeof sourceAddress !== "string") {
        throw createDgramArgTypeError("sourceAddress", "string", sourceAddress);
      }
      if (typeof groupAddress !== "string") {
        throw createDgramArgTypeError("groupAddress", "string", groupAddress);
      }
      const validatedSource = validateDgramSourceAddress(
        this._type,
        "addSourceSpecificMembership",
        sourceAddress
      );
      const validatedGroup = validateDgramMulticastAddress(
        this._type,
        "addSourceSpecificMembership",
        groupAddress
      );
      if (multicastInterface !== void 0 && typeof multicastInterface !== "string") {
        throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
      }
      this._memberships.add(`${validatedSource}>${validatedGroup}|${multicastInterface ?? ""}`);
    }
    dropSourceSpecificMembership(sourceAddress, groupAddress, multicastInterface) {
      if (this._closed) {
        throw createDgramNotRunningError();
      }
      if (typeof sourceAddress !== "string") {
        throw createDgramArgTypeError("sourceAddress", "string", sourceAddress);
      }
      if (typeof groupAddress !== "string") {
        throw createDgramArgTypeError("groupAddress", "string", groupAddress);
      }
      const validatedSource = validateDgramSourceAddress(
        this._type,
        "dropSourceSpecificMembership",
        sourceAddress
      );
      const validatedGroup = validateDgramMulticastAddress(
        this._type,
        "dropSourceSpecificMembership",
        groupAddress
      );
      if (multicastInterface !== void 0 && typeof multicastInterface !== "string") {
        throw createDgramArgTypeError("multicastInterface", "string", multicastInterface);
      }
      const membershipKey = `${validatedSource}>${validatedGroup}|${multicastInterface ?? ""}`;
      if (!this._memberships.has(membershipKey)) {
        throw createDgramSyscallError("dropSourceSpecificMembership", "EADDRNOTAVAIL");
      }
      this._memberships.delete(membershipKey);
    }
    setMulticastInterface(interfaceAddress) {
      if (typeof interfaceAddress !== "string") {
        throw createDgramArgTypeError("interfaceAddress", "string", interfaceAddress);
      }
      if (this._closed) {
        throw createDgramNotRunningError();
      }
      this._ensureBoundForSocketOption("setMulticastInterface");
      if (this._type === "udp4") {
        if (interfaceAddress === "0.0.0.0") {
          this._multicastInterface = interfaceAddress;
          return;
        }
        if (!isIPv4String(interfaceAddress)) {
          throw createDgramSyscallError("setMulticastInterface", "ENOPROTOOPT");
        }
        if (!isIPv4UnicastAddress(interfaceAddress)) {
          throw createDgramSyscallError("setMulticastInterface", "EADDRNOTAVAIL");
        }
        this._multicastInterface = interfaceAddress;
        return;
      }
      if (interfaceAddress === "" || interfaceAddress === "undefined" || !isIPv6String(interfaceAddress)) {
        throw createDgramSyscallError("setMulticastInterface", "EINVAL");
      }
      this._multicastInterface = interfaceAddress;
    }
    on(event, listener) {
      if (!this._listeners[event]) this._listeners[event] = [];
      this._listeners[event].push(listener);
      return this;
    }
    addListener(event, listener) {
      return this.on(event, listener);
    }
    once(event, listener) {
      if (!this._onceListeners[event]) this._onceListeners[event] = [];
      this._onceListeners[event].push(listener);
      return this;
    }
    removeListener(event, listener) {
      const listeners = this._listeners[event];
      if (listeners) {
        const index = listeners.indexOf(listener);
        if (index >= 0) listeners.splice(index, 1);
      }
      const onceListeners = this._onceListeners[event];
      if (onceListeners) {
        const index = onceListeners.indexOf(listener);
        if (index >= 0) onceListeners.splice(index, 1);
      }
      return this;
    }
    off(event, listener) {
      return this.removeListener(event, listener);
    }
    emit(event, ...args) {
      return this._emit(event, ...args);
    }
    async _bindInternal(port, address, callback) {
      if (this._closed) {
        return;
      }
      if (this._bound || this._bindPromise) {
        throw createDgramAlreadyBoundError();
      }
      if (typeof _dgramSocketBindRaw === "undefined") {
        throw new Error("dgram.bind is not supported in sandbox");
      }
      this._bindPromise = (async () => {
        try {
          normalizeDgramBridgeResult(_dgramSocketBindRaw.applySyncPromise(void 0, [
            this._socketId,
            { port, address }
          ]));
          this._bound = true;
          this._applyInitialBufferSizes();
          this._syncHandleRef();
          queueMicrotask(() => {
            if (this._closed) {
              return;
            }
            this._emit("listening");
            callback?.call(this);
            void this._pumpMessages();
          });
        } catch (error) {
          queueMicrotask(() => {
            this._emit("error", error);
          });
          throw error;
        } finally {
          this._bindPromise = null;
        }
      })();
      return this._bindPromise;
    }
    async _ensureBound() {
      if (this._bound) {
        return;
      }
      if (this._bindPromise) {
        await this._bindPromise;
        return;
      }
      await this._bindInternal(0, this._type === "udp6" ? "::" : "0.0.0.0");
    }
    async _sendInternal(data, port, address, callback) {
      try {
        await this._ensureBound();
        if (this._closed || typeof _dgramSocketSendRaw === "undefined") {
          return;
        }
        const result = normalizeDgramBridgeResult(_dgramSocketSendRaw.applySyncPromise(void 0, [
          this._socketId,
          data,
          { port, address }
        ]));
        if (callback) {
          queueMicrotask(() => {
            callback(null, typeof result?.bytes === "number" ? result.bytes : data.length);
          });
        }
      } catch (error) {
        if (callback) {
          queueMicrotask(() => {
            callback(error);
          });
          return;
        }
        queueMicrotask(() => {
          this._emit("error", error);
        });
      }
    }
    async _pumpMessages() {
      if (this._receiveLoopRunning || this._closed || !this._bound) {
        return;
      }
      if (typeof _dgramSocketRecvRaw === "undefined") {
        return;
      }
      this._receiveLoopRunning = true;
      try {
        while (!this._closed && this._bound) {
          const payload = normalizeDgramBridgeResult(
            _dgramSocketRecvRaw.applySync(void 0, [this._socketId, NET_BRIDGE_POLL_DELAY_MS])
          );
          if (payload === NET_BRIDGE_TIMEOUT_SENTINEL || !payload) {
            this._receivePollTimer = setTimeout(() => {
              this._receivePollTimer = null;
              void this._pumpMessages();
            }, NET_BRIDGE_POLL_DELAY_MS);
            if (!this._refed && typeof this._receivePollTimer.unref === "function") {
              this._receivePollTimer.unref();
            }
            return;
          }
          if (payload.type === "message") {
            const message = decodeDgramBridgeBytes(payload.data);
            this._emit("message", message, {
              address: payload.remoteAddress,
              family: payload.remoteFamily ?? socketFamilyForAddress(payload.remoteAddress),
              port: payload.remotePort,
              size: message.length
            });
            continue;
          }
          if (payload.type === "error") {
            const error = new Error(
              typeof payload.message === "string" ? payload.message : "Agent OS dgram socket error"
            );
            if (typeof payload.code === "string" && payload.code.length > 0) {
              error.code = payload.code;
            }
            this._emit("error", error);
          }
        }
      } catch (error) {
        this._emit("error", error);
      } finally {
        this._receiveLoopRunning = false;
      }
    }
    _clearReceivePollTimer() {
      if (this._receivePollTimer) {
        clearTimeout(this._receivePollTimer);
        this._receivePollTimer = null;
      }
    }
    _ensureBoundForSocketOption(syscall) {
      if (!this._bound || this._closed) {
        throw createDgramSyscallError(syscall, "EBADF");
      }
    }
    _setBufferSize(which, size, requireRunning = true) {
      if (!Number.isInteger(size) || size <= 0 || !Number.isFinite(size)) {
        throw createDgramBufferSizeTypeError();
      }
      if (size > 2147483647) {
        throw createDgramBufferSizeSystemError(which, "EINVAL");
      }
      if (requireRunning && (!this._bound || this._closed)) {
        throw createDgramBufferSizeSystemError(which, "EBADF");
      }
      if (typeof _dgramSocketSetBufferSizeRaw !== "undefined" && this._bound && !this._closed) {
        _dgramSocketSetBufferSizeRaw.applySync(void 0, [this._socketId, which, size]);
      }
      if (which === "recv") {
        this._recvBufferSize = size;
        return;
      }
      this._sendBufferSize = size;
    }
    _getBufferSize(which) {
      if (!this._bound || this._closed) {
        throw createDgramBufferSizeSystemError(which, "EBADF");
      }
      const fallback = which === "recv" ? this._recvBufferSize ?? 0 : this._sendBufferSize ?? 0;
      if (typeof _dgramSocketGetBufferSizeRaw === "undefined") {
        return getPlatformDgramBufferSize(fallback);
      }
      const rawSize = _dgramSocketGetBufferSizeRaw.applySync(void 0, [this._socketId, which]);
      return getPlatformDgramBufferSize(rawSize > 0 ? rawSize : fallback);
    }
    _applyInitialBufferSizes() {
      if (this._recvBufferSize !== void 0) {
        this._setBufferSize("recv", this._recvBufferSize);
      }
      if (this._sendBufferSize !== void 0) {
        this._setBufferSize("send", this._sendBufferSize);
      }
    }
    _syncHandleRef() {
      if (!this._bound || this._closed || !this._refed) {
        if (this._handleRefId && typeof _unregisterHandle === "function") {
          _unregisterHandle(this._handleRefId);
        }
        this._handleRefId = null;
        return;
      }
      const nextHandleId = `${DGRAM_HANDLE_PREFIX}${this._socketId}`;
      if (this._handleRefId === nextHandleId) {
        return;
      }
      if (this._handleRefId && typeof _unregisterHandle === "function") {
        _unregisterHandle(this._handleRefId);
      }
      this._handleRefId = nextHandleId;
      if (typeof _registerHandle === "function") {
        _registerHandle(this._handleRefId, "dgram socket");
      }
    }
    _emit(event, ...args) {
      let handled = false;
      const listeners = this._listeners[event];
      if (listeners) {
        for (const listener of [...listeners]) {
          listener(...args);
          handled = true;
        }
      }
      const onceListeners = this._onceListeners[event];
      if (onceListeners) {
        this._onceListeners[event] = [];
        for (const listener of [...onceListeners]) {
          listener(...args);
          handled = true;
        }
      }
      return handled;
    }
  };
  var dgramModule = {
    Socket: DgramSocket,
    createSocket(optionsOrType, callback) {
      return new DgramSocket(optionsOrType, callback);
    }
  };
  function isSqlitePlainObject(value) {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      return false;
    }
    if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
      return false;
    }
    const prototype = Object.getPrototypeOf(value);
    return prototype === Object.prototype || prototype === null;
  }
  function encodeSqliteValue(value) {
    if (value === null || value === void 0 || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
      return value ?? null;
    }
    if (typeof value === "bigint") {
      return {
        __agentosSqliteType: "bigint",
        value: value.toString()
      };
    }
    if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
      return {
        __agentosSqliteType: "uint8array",
        value: Buffer.from(value).toString("base64")
      };
    }
    if (Array.isArray(value)) {
      return value.map((entry) => encodeSqliteValue(entry));
    }
    if (value && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value).map(([key, entry]) => [key, encodeSqliteValue(entry)])
      );
    }
    return null;
  }
  function decodeSqliteValue(value) {
    if (value === null || value === void 0 || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
      return value ?? null;
    }
    if (Array.isArray(value)) {
      return value.map((entry) => decodeSqliteValue(entry));
    }
    if (value && typeof value === "object") {
      if (value.__agentosSqliteType === "bigint" && typeof value.value === "string") {
        return BigInt(value.value);
      }
      if (value.__agentosSqliteType === "uint8array" && typeof value.value === "string") {
        return Buffer.from(value.value, "base64");
      }
      return Object.fromEntries(
        Object.entries(value).map(([key, entry]) => [key, decodeSqliteValue(entry)])
      );
    }
    return value;
  }
  function normalizeSqliteParams(params) {
    if (!Array.isArray(params) || params.length === 0) {
      return null;
    }
    if (params.length === 1 && isSqlitePlainObject(params[0])) {
      return encodeSqliteValue(params[0]);
    }
    return params.map((entry) => encodeSqliteValue(entry));
  }
  function sqliteBridgeCall(bridgeFn, args, label) {
    if (typeof bridgeFn === "function") {
      return decodeSqliteValue(bridgeFn(...args));
    }
    if (!bridgeFn) {
      throw new Error(`sqlite bridge is not available for ${label}`);
    }
    if (typeof bridgeFn.applySync === "function") {
      return decodeSqliteValue(bridgeFn.applySync(void 0, args));
    }
    if (typeof bridgeFn.applySyncPromise === "function") {
      return decodeSqliteValue(bridgeFn.applySyncPromise(void 0, args));
    }
    throw new Error(`sqlite bridge is not available for ${label}`);
  }
  var _sqliteConstants = createBridgeSyncFacade("_sqliteConstantsRaw");
  var _sqliteDatabaseOpen = createBridgeSyncFacade("_sqliteDatabaseOpenRaw");
  var _sqliteDatabaseClose = createBridgeSyncFacade("_sqliteDatabaseCloseRaw");
  var _sqliteDatabaseExec = createBridgeSyncFacade("_sqliteDatabaseExecRaw");
  var _sqliteDatabaseQuery = createBridgeSyncFacade("_sqliteDatabaseQueryRaw");
  var _sqliteDatabasePrepare = createBridgeSyncFacade("_sqliteDatabasePrepareRaw");
  var _sqliteDatabaseLocation = createBridgeSyncFacade("_sqliteDatabaseLocationRaw");
  var _sqliteDatabaseCheckpoint = createBridgeSyncFacade("_sqliteDatabaseCheckpointRaw");
  var _sqliteStatementRun = createBridgeSyncFacade("_sqliteStatementRunRaw");
  var _sqliteStatementGet = createBridgeSyncFacade("_sqliteStatementGetRaw");
  var _sqliteStatementAll = createBridgeSyncFacade("_sqliteStatementAllRaw");
  var _sqliteStatementColumns = createBridgeSyncFacade("_sqliteStatementColumnsRaw");
  var _sqliteStatementSetReturnArrays = createBridgeSyncFacade("_sqliteStatementSetReturnArraysRaw");
  var _sqliteStatementSetReadBigInts = createBridgeSyncFacade("_sqliteStatementSetReadBigIntsRaw");
  var _sqliteStatementSetAllowBareNamedParameters = createBridgeSyncFacade("_sqliteStatementSetAllowBareNamedParametersRaw");
  var _sqliteStatementSetAllowUnknownNamedParameters = createBridgeSyncFacade("_sqliteStatementSetAllowUnknownNamedParametersRaw");
  var _sqliteStatementFinalize = createBridgeSyncFacade("_sqliteStatementFinalizeRaw");
  var StatementSync = class {
    constructor(database, statementId) {
      this._database = database;
      this._statementId = statementId;
      this._finalized = false;
    }
    _assertOpen() {
      this._database._assertOpen();
      if (this._finalized) {
        throw new Error("SQLite statement is already finalized");
      }
    }
    run(...params) {
      this._assertOpen();
      return sqliteBridgeCall(
        _sqliteStatementRun,
        [this._statementId, normalizeSqliteParams(params)],
        "statement.run"
      );
    }
    get(...params) {
      this._assertOpen();
      return sqliteBridgeCall(
        _sqliteStatementGet,
        [this._statementId, normalizeSqliteParams(params)],
        "statement.get"
      );
    }
    all(...params) {
      this._assertOpen();
      return sqliteBridgeCall(
        _sqliteStatementAll,
        [this._statementId, normalizeSqliteParams(params)],
        "statement.all"
      );
    }
    iterate(...params) {
      const rows = this.all(...params);
      return rows[Symbol.iterator]();
    }
    columns() {
      this._assertOpen();
      return sqliteBridgeCall(
        _sqliteStatementColumns,
        [this._statementId],
        "statement.columns"
      );
    }
    setReturnArrays(enabled) {
      this._assertOpen();
      sqliteBridgeCall(
        _sqliteStatementSetReturnArrays,
        [this._statementId, Boolean(enabled)],
        "statement.setReturnArrays"
      );
    }
    setReadBigInts(enabled) {
      this._assertOpen();
      sqliteBridgeCall(
        _sqliteStatementSetReadBigInts,
        [this._statementId, Boolean(enabled)],
        "statement.setReadBigInts"
      );
    }
    setAllowBareNamedParameters(enabled) {
      this._assertOpen();
      sqliteBridgeCall(
        _sqliteStatementSetAllowBareNamedParameters,
        [this._statementId, Boolean(enabled)],
        "statement.setAllowBareNamedParameters"
      );
    }
    setAllowUnknownNamedParameters(enabled) {
      this._assertOpen();
      sqliteBridgeCall(
        _sqliteStatementSetAllowUnknownNamedParameters,
        [this._statementId, Boolean(enabled)],
        "statement.setAllowUnknownNamedParameters"
      );
    }
    finalize() {
      if (this._finalized) {
        return null;
      }
      this._database._assertOpen();
      sqliteBridgeCall(
        _sqliteStatementFinalize,
        [this._statementId],
        "statement.finalize"
      );
      this._finalized = true;
      return null;
    }
  };
  var DatabaseSync = class {
    constructor(location = ":memory:", options = void 0) {
      this._closed = false;
      this._databaseId = sqliteBridgeCall(
        _sqliteDatabaseOpen,
        [typeof location === "string" ? location : ":memory:", options ?? null],
        "database.open"
      );
    }
    _assertOpen() {
      if (this._closed) {
        throw new Error("SQLite database is already closed");
      }
    }
    close() {
      if (this._closed) {
        return null;
      }
      sqliteBridgeCall(
        _sqliteDatabaseClose,
        [this._databaseId],
        "database.close"
      );
      this._closed = true;
      return null;
    }
    exec(sql) {
      this._assertOpen();
      return sqliteBridgeCall(
        _sqliteDatabaseExec,
        [this._databaseId, String(sql ?? "")],
        "database.exec"
      );
    }
    query(sql, params = null, options = null) {
      this._assertOpen();
      const normalized = params === null ? null : normalizeSqliteParams(Array.isArray(params) ? params : [params]);
      return sqliteBridgeCall(
        _sqliteDatabaseQuery,
        [this._databaseId, String(sql ?? ""), normalized, options ?? null],
        "database.query"
      );
    }
    prepare(sql) {
      this._assertOpen();
      const statementId = sqliteBridgeCall(
        _sqliteDatabasePrepare,
        [this._databaseId, String(sql ?? "")],
        "database.prepare"
      );
      return new StatementSync(this, statementId);
    }
    location() {
      this._assertOpen();
      return sqliteBridgeCall(
        _sqliteDatabaseLocation,
        [this._databaseId],
        "database.location"
      );
    }
    checkpoint() {
      this._assertOpen();
      return sqliteBridgeCall(
        _sqliteDatabaseCheckpoint,
        [this._databaseId],
        "database.checkpoint"
      );
    }
  };
  DatabaseSync.prototype[Symbol.dispose] = DatabaseSync.prototype.close;
  StatementSync.prototype[Symbol.dispose] = StatementSync.prototype.finalize;
  var sqliteConstants;
  function getSqliteConstants() {
    if (sqliteConstants === void 0) {
      sqliteConstants = Object.freeze(
        sqliteBridgeCall(_sqliteConstants, [], "constants") ?? {}
      );
    }
    return sqliteConstants;
  }
  var sqliteModule = {
    DatabaseSync,
    StatementSync,
    get constants() {
      return getSqliteConstants();
    }
  };
  exposeCustomGlobal("_netModule", netModule);
  exposeCustomGlobal("_tlsModule", tlsModule);
  exposeCustomGlobal("_dgramModule", dgramModule);
  exposeCustomGlobal("_sqliteModule", sqliteModule);
  var network_default = {
    fetch,
    Headers,
    Request,
    Response,
    dns,
    http,
    https,
    http2,
    IncomingMessage,
    ClientRequest,
    net: netModule,
    tls: tlsModule,
    dgram: dgramModule
  };

  // .agent/recovery/secure-exec/nodejs/src/bridge/whatwg-url.ts
  var inspectCustomSymbol = /* @__PURE__ */ Symbol.for("nodejs.util.inspect.custom");
  var toStringTagSymbol = Symbol.toStringTag;
  var ERR_INVALID_THIS = "ERR_INVALID_THIS";
  var ERR_MISSING_ARGS = "ERR_MISSING_ARGS";
  var ERR_INVALID_URL = "ERR_INVALID_URL";
  var ERR_ARG_NOT_ITERABLE = "ERR_ARG_NOT_ITERABLE";
  var ERR_INVALID_TUPLE = "ERR_INVALID_TUPLE";
  var URL_SEARCH_PARAMS_TYPE = "URLSearchParams";
  var kLinkedSearchParams = /* @__PURE__ */ Symbol("secureExecLinkedURLSearchParams");
  var kBlobUrlStore = /* @__PURE__ */ Symbol.for("secureExec.blobUrlStore");
  var kBlobUrlCounter = /* @__PURE__ */ Symbol.for("secureExec.blobUrlCounter");
  var SEARCH_PARAM_METHOD_NAMES = ["append", "delete", "get", "getAll", "has"];
  var SEARCH_PARAM_PAIR_METHOD_NAMES = ["append", "set"];
  var URL_SCHEME_TYPES = {
    "http:": 0,
    "https:": 2,
    "ws:": 4,
    "wss:": 5,
    "file:": 6,
    "ftp:": 8
  };
  var searchParamsBrand = /* @__PURE__ */ new WeakSet();
  var searchParamsState = /* @__PURE__ */ new WeakMap();
  var searchParamsIteratorBrand = /* @__PURE__ */ new WeakSet();
  var searchParamsIteratorState = /* @__PURE__ */ new WeakMap();
  function createNodeTypeError(message, code) {
    const error = new TypeError(message);
    error.code = code;
    return error;
  }
  function createInvalidUrlError() {
    const error = new TypeError("Invalid URL");
    error.code = ERR_INVALID_URL;
    return error;
  }
  function createUrlReceiverTypeError() {
    return new TypeError("Receiver must be an instance of class URL");
  }
  function createMissingArgsError(message) {
    return createNodeTypeError(message, ERR_MISSING_ARGS);
  }
  function createIterableTypeError() {
    return createNodeTypeError("Query pairs must be iterable", ERR_ARG_NOT_ITERABLE);
  }
  function createTupleTypeError() {
    return createNodeTypeError(
      "Each query pair must be an iterable [name, value] tuple",
      ERR_INVALID_TUPLE
    );
  }
  function createSymbolStringError() {
    return new TypeError("Cannot convert a Symbol value to a string");
  }
  function toNodeString(value) {
    if (typeof value === "symbol") {
      throw createSymbolStringError();
    }
    return String(value);
  }
  function toWellFormedString(value) {
    let result = "";
    for (let index = 0; index < value.length; index += 1) {
      const codeUnit = value.charCodeAt(index);
      if (codeUnit >= 55296 && codeUnit <= 56319) {
        const nextIndex = index + 1;
        if (nextIndex < value.length) {
          const nextCodeUnit = value.charCodeAt(nextIndex);
          if (nextCodeUnit >= 56320 && nextCodeUnit <= 57343) {
            result += value[index] + value[nextIndex];
            index = nextIndex;
            continue;
          }
        }
        result += "\uFFFD";
        continue;
      }
      if (codeUnit >= 56320 && codeUnit <= 57343) {
        result += "\uFFFD";
        continue;
      }
      result += value[index];
    }
    return result;
  }
  function toNodeUSVString(value) {
    return toWellFormedString(toNodeString(value));
  }
  function assertUrlSearchParamsReceiver(receiver) {
    if (!searchParamsBrand.has(receiver)) {
      throw createNodeTypeError(
        'Value of "this" must be of type URLSearchParams',
        ERR_INVALID_THIS
      );
    }
  }
  function assertUrlSearchParamsIteratorReceiver(receiver) {
    if (!searchParamsIteratorBrand.has(receiver)) {
      throw createNodeTypeError(
        'Value of "this" must be of type URLSearchParamsIterator',
        ERR_INVALID_THIS
      );
    }
  }
  function getUrlSearchParamsImpl(receiver) {
    const state = searchParamsState.get(receiver);
    if (!state) {
      throw createNodeTypeError(
        'Value of "this" must be of type URLSearchParams',
        ERR_INVALID_THIS
      );
    }
    return state.getImpl();
  }
  function countSearchParams(params) {
    let count = 0;
    for (const _entry of params) {
      count++;
    }
    return count;
  }
  function normalizeSearchParamsInit(init) {
    if (init && typeof init === "object" && kLinkedSearchParams in init) {
      return init;
    }
    if (init == null) {
      return void 0;
    }
    if (typeof init === "string") {
      return toNodeUSVString(init);
    }
    if (typeof init === "object" || typeof init === "function") {
      const iterator2 = init[Symbol.iterator];
      if (iterator2 !== void 0) {
        if (typeof iterator2 !== "function") {
          throw createIterableTypeError();
        }
        const pairs2 = [];
        for (const pair of init) {
          if (pair == null) {
            throw createTupleTypeError();
          }
          const pairIterator = pair[Symbol.iterator];
          if (typeof pairIterator !== "function") {
            throw createTupleTypeError();
          }
          const values = Array.from(pair);
          if (values.length !== 2) {
            throw createTupleTypeError();
          }
          pairs2.push([toNodeUSVString(values[0]), toNodeUSVString(values[1])]);
        }
        return pairs2;
      }
      const pairs = [];
      for (const key of Reflect.ownKeys(init)) {
        if (!Object.prototype.propertyIsEnumerable.call(init, key)) {
          continue;
        }
        pairs.push([
          toNodeUSVString(key),
          toNodeUSVString(init[key])
        ]);
      }
      return pairs;
    }
    return toNodeUSVString(init);
  }
  var NativeURLSearchParams = typeof globalThis.URLSearchParams === "function" ? globalThis.URLSearchParams : class URLSearchParams {
    constructor(init = "") {
      this._pairs = [];
      if (typeof init === "string") {
        const query = init.replace(/^\?/, "");
        if (!query) {
          return;
        }
        for (const entry of query.split("&")) {
          if (!entry) {
            continue;
          }
          const [key, ...rest] = entry.split("=");
          this._pairs.push([decodeURIComponent(key), decodeURIComponent(rest.join("="))]);
        }
        return;
      }
      if (Array.isArray(init)) {
        for (const pair of init) {
          if (pair == null || pair.length !== 2) {
            continue;
          }
          this._pairs.push([String(pair[0]), String(pair[1])]);
        }
        return;
      }
      if (init && typeof init === "object") {
        for (const [key, value] of Object.entries(init)) {
          this._pairs.push([String(key), String(value)]);
        }
      }
    }
    append(name, value) {
      this._pairs.push([String(name), String(value)]);
    }
    delete(name) {
      const key = String(name);
      this._pairs = this._pairs.filter(([candidate]) => candidate !== key);
    }
    get(name) {
      const key = String(name);
      const match = this._pairs.find(([candidate]) => candidate === key);
      return match ? match[1] : null;
    }
    getAll(name) {
      const key = String(name);
      return this._pairs.filter(([candidate]) => candidate === key).map(([, value]) => value);
    }
    has(name) {
      const key = String(name);
      return this._pairs.some(([candidate]) => candidate === key);
    }
    set(name, value) {
      const key = String(name);
      const stringValue = String(value);
      let replaced = false;
      this._pairs = this._pairs.filter(([candidate]) => {
        if (candidate !== key) {
          return true;
        }
        if (!replaced) {
          replaced = true;
          return false;
        }
        return false;
      });
      this._pairs.push([key, stringValue]);
    }
    sort() {
      this._pairs.sort(([left], [right]) => left.localeCompare(right));
    }
    entries() {
      return this._pairs[Symbol.iterator]();
    }
    keys() {
      return this._pairs.map(([key]) => key)[Symbol.iterator]();
    }
    values() {
      return this._pairs.map(([, value]) => value)[Symbol.iterator]();
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    toString() {
      return this._pairs.map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`).join("&");
    }
  };
  function createStandaloneSearchParams(init) {
    if (typeof init === "string") {
      return new NativeURLSearchParams(init);
    }
    return init === void 0 ? new NativeURLSearchParams() : new NativeURLSearchParams(init);
  }
  function createCollectionBody(items, options, emptyBody) {
    if (items.length === 0) {
      return emptyBody;
    }
    const oneLine = `{ ${items.join(", ")} }`;
    const breakLength = options?.breakLength ?? Infinity;
    if (oneLine.length <= breakLength) {
      return oneLine;
    }
    return `{
  ${items.join(",\n  ")} }`;
  }
  function createUrlContext(url) {
    const href = url.href;
    const protocolEnd = href.indexOf(":") + 1;
    const authIndex = href.indexOf("@");
    const pathnameStart = href.indexOf("/", protocolEnd + 2);
    const searchStart = href.indexOf("?");
    const hashStart = href.indexOf("#");
    const usernameEnd = url.username.length > 0 ? href.indexOf(":", protocolEnd + 2) : protocolEnd + 2;
    const hostStart = authIndex === -1 ? protocolEnd + 2 : authIndex;
    const hostEnd = pathnameStart === -1 ? href.length : pathnameStart - (url.port.length > 0 ? url.port.length + 1 : 0);
    const port = url.port.length > 0 ? Number(url.port) : null;
    return {
      href,
      protocol_end: protocolEnd,
      username_end: usernameEnd,
      host_start: hostStart,
      host_end: hostEnd,
      pathname_start: pathnameStart === -1 ? href.length : pathnameStart,
      search_start: searchStart === -1 ? href.length : searchStart,
      hash_start: hashStart === -1 ? href.length : hashStart,
      port,
      scheme_type: URL_SCHEME_TYPES[url.protocol] ?? 1,
      hasPort: url.port.length > 0,
      hasSearch: url.search.length > 0,
      hasHash: url.hash.length > 0
    };
  }
  function formatUrlContext(url, inspect, options) {
    const context = createUrlContext(url);
    const formatValue = typeof inspect === "function" ? (value) => inspect(value, options) : (value) => JSON.stringify(value);
    const portValue = context.port === null ? "null" : String(context.port);
    return [
      "URLContext {",
      `    href: ${formatValue(context.href)},`,
      `    protocol_end: ${context.protocol_end},`,
      `    username_end: ${context.username_end},`,
      `    host_start: ${context.host_start},`,
      `    host_end: ${context.host_end},`,
      `    pathname_start: ${context.pathname_start},`,
      `    search_start: ${context.search_start},`,
      `    hash_start: ${context.hash_start},`,
      `    port: ${portValue},`,
      `    scheme_type: ${context.scheme_type},`,
      "    [hasPort]: [Getter],",
      "    [hasSearch]: [Getter],",
      "    [hasHash]: [Getter]",
      "  }"
    ].join("\n");
  }
  function getBlobUrlStore() {
    const globalRecord = globalThis;
    const existing = globalRecord[kBlobUrlStore];
    if (existing instanceof Map) {
      return existing;
    }
    const store = /* @__PURE__ */ new Map();
    globalRecord[kBlobUrlStore] = store;
    return store;
  }
  function nextBlobUrlId() {
    const globalRecord = globalThis;
    const nextId = typeof globalRecord[kBlobUrlCounter] === "number" ? globalRecord[kBlobUrlCounter] : 1;
    globalRecord[kBlobUrlCounter] = nextId + 1;
    return nextId;
  }
  var URLSearchParamsIterator = class _URLSearchParamsIterator {
    constructor(values) {
      searchParamsIteratorBrand.add(this);
      searchParamsIteratorState.set(this, { values, index: 0 });
    }
    next() {
      assertUrlSearchParamsIteratorReceiver(this);
      const state = searchParamsIteratorState.get(this);
      if (state.index >= state.values.length) {
        return { value: void 0, done: true };
      }
      const value = state.values[state.index];
      state.index += 1;
      return { value, done: false };
    }
    [inspectCustomSymbol](depth, options, inspect) {
      assertUrlSearchParamsIteratorReceiver(this);
      if (depth < 0) {
        return "[Object]";
      }
      const state = searchParamsIteratorState.get(this);
      const formatValue = typeof inspect === "function" ? (value) => inspect(value, options) : (value) => JSON.stringify(value);
      const remaining = state.values.slice(state.index).map((value) => formatValue(value));
      return `URLSearchParams Iterator ${createCollectionBody(remaining, options, "{  }")}`;
    }
    get [toStringTagSymbol]() {
      if (this !== _URLSearchParamsIterator.prototype) {
        assertUrlSearchParamsIteratorReceiver(this);
      }
      return "URLSearchParams Iterator";
    }
  };
  Object.defineProperties(URLSearchParamsIterator.prototype, {
    next: {
      value: URLSearchParamsIterator.prototype.next,
      writable: true,
      configurable: true,
      enumerable: true
    },
    [Symbol.iterator]: {
      value: function iterator() {
        assertUrlSearchParamsIteratorReceiver(this);
        return this;
      },
      writable: true,
      configurable: true,
      enumerable: false
    },
    [inspectCustomSymbol]: {
      value: URLSearchParamsIterator.prototype[inspectCustomSymbol],
      writable: true,
      configurable: true,
      enumerable: false
    },
    [toStringTagSymbol]: {
      get: Object.getOwnPropertyDescriptor(URLSearchParamsIterator.prototype, toStringTagSymbol)?.get,
      configurable: true,
      enumerable: false
    }
  });
  Object.defineProperty(
    Object.getOwnPropertyDescriptor(URLSearchParamsIterator.prototype, Symbol.iterator)?.value,
    "name",
    {
      value: "entries",
      configurable: true
    }
  );
  var URLSearchParams = class _URLSearchParams {
    constructor(init) {
      searchParamsBrand.add(this);
      const normalized = normalizeSearchParamsInit(init);
      if (normalized && typeof normalized === "object" && kLinkedSearchParams in normalized) {
        searchParamsState.set(this, {
          getImpl: normalized[kLinkedSearchParams]
        });
        return;
      }
      const impl = createStandaloneSearchParams(
        normalized
      );
      searchParamsState.set(this, { getImpl: () => impl });
    }
    append(name, value) {
      assertUrlSearchParamsReceiver(this);
      if (arguments.length < 2) {
        throw createMissingArgsError('The "name" and "value" arguments must be specified');
      }
      getUrlSearchParamsImpl(this).append(toNodeUSVString(name), toNodeUSVString(value));
    }
    delete(name) {
      assertUrlSearchParamsReceiver(this);
      if (arguments.length < 1) {
        throw createMissingArgsError('The "name" argument must be specified');
      }
      getUrlSearchParamsImpl(this).delete(toNodeUSVString(name));
    }
    get(name) {
      assertUrlSearchParamsReceiver(this);
      if (arguments.length < 1) {
        throw createMissingArgsError('The "name" argument must be specified');
      }
      return getUrlSearchParamsImpl(this).get(toNodeUSVString(name));
    }
    getAll(name) {
      assertUrlSearchParamsReceiver(this);
      if (arguments.length < 1) {
        throw createMissingArgsError('The "name" argument must be specified');
      }
      return getUrlSearchParamsImpl(this).getAll(toNodeUSVString(name));
    }
    has(name) {
      assertUrlSearchParamsReceiver(this);
      if (arguments.length < 1) {
        throw createMissingArgsError('The "name" argument must be specified');
      }
      return getUrlSearchParamsImpl(this).has(toNodeUSVString(name));
    }
    set(name, value) {
      assertUrlSearchParamsReceiver(this);
      if (arguments.length < 2) {
        throw createMissingArgsError('The "name" and "value" arguments must be specified');
      }
      getUrlSearchParamsImpl(this).set(toNodeUSVString(name), toNodeUSVString(value));
    }
    sort() {
      assertUrlSearchParamsReceiver(this);
      getUrlSearchParamsImpl(this).sort();
    }
    entries() {
      assertUrlSearchParamsReceiver(this);
      return new URLSearchParamsIterator(Array.from(getUrlSearchParamsImpl(this)));
    }
    keys() {
      assertUrlSearchParamsReceiver(this);
      return new URLSearchParamsIterator(Array.from(getUrlSearchParamsImpl(this).keys()));
    }
    values() {
      assertUrlSearchParamsReceiver(this);
      return new URLSearchParamsIterator(Array.from(getUrlSearchParamsImpl(this).values()));
    }
    forEach(callback, thisArg) {
      assertUrlSearchParamsReceiver(this);
      if (typeof callback !== "function") {
        throw createNodeTypeError(
          'The "callback" argument must be of type function. Received ' + (callback === void 0 ? "undefined" : typeof callback),
          "ERR_INVALID_ARG_TYPE"
        );
      }
      for (const [key, value] of getUrlSearchParamsImpl(this)) {
        callback.call(thisArg, value, key, this);
      }
    }
    toString() {
      assertUrlSearchParamsReceiver(this);
      return getUrlSearchParamsImpl(this).toString();
    }
    get size() {
      assertUrlSearchParamsReceiver(this);
      return countSearchParams(getUrlSearchParamsImpl(this));
    }
    [inspectCustomSymbol](depth, options, inspect) {
      assertUrlSearchParamsReceiver(this);
      if (depth < 0) {
        return "[Object]";
      }
      const formatValue = typeof inspect === "function" ? (value) => inspect(value, options) : (value) => JSON.stringify(value);
      const items = Array.from(
        getUrlSearchParamsImpl(this)
      ).map(
        ([key, value]) => `${formatValue(key)} => ${formatValue(value)}`
      );
      return `URLSearchParams ${createCollectionBody(items, options, "{}")}`;
    }
    get [toStringTagSymbol]() {
      if (this !== _URLSearchParams.prototype) {
        assertUrlSearchParamsReceiver(this);
      }
      return URL_SEARCH_PARAMS_TYPE;
    }
  };
  for (const name of SEARCH_PARAM_METHOD_NAMES) {
    Object.defineProperty(URLSearchParams.prototype, name, {
      value: URLSearchParams.prototype[name],
      writable: true,
      configurable: true,
      enumerable: true
    });
  }
  for (const name of SEARCH_PARAM_PAIR_METHOD_NAMES) {
    Object.defineProperty(URLSearchParams.prototype, name, {
      value: URLSearchParams.prototype[name],
      writable: true,
      configurable: true,
      enumerable: true
    });
  }
  for (const name of ["sort", "entries", "forEach", "keys", "values", "toString"]) {
    Object.defineProperty(URLSearchParams.prototype, name, {
      value: URLSearchParams.prototype[name],
      writable: true,
      configurable: true,
      enumerable: true
    });
  }
  Object.defineProperties(URLSearchParams.prototype, {
    size: {
      get: Object.getOwnPropertyDescriptor(URLSearchParams.prototype, "size")?.get,
      configurable: true,
      enumerable: true
    },
    [Symbol.iterator]: {
      value: URLSearchParams.prototype.entries,
      writable: true,
      configurable: true,
      enumerable: false
    },
    [inspectCustomSymbol]: {
      value: URLSearchParams.prototype[inspectCustomSymbol],
      writable: true,
      configurable: true,
      enumerable: false
    },
    [toStringTagSymbol]: {
      get: Object.getOwnPropertyDescriptor(URLSearchParams.prototype, toStringTagSymbol)?.get,
      configurable: true,
      enumerable: false
    }
  });
  var NativeURL = typeof globalThis.URL === "function" ? globalThis.URL : class URL {
    constructor(url, base) {
      const raw = String(url ?? "");
      const hasScheme = /^[a-zA-Z][a-zA-Z\d+\-.]*:/.test(raw);
      const baseHref = hasScheme || typeof base === "undefined" ? "" : String(new URL(base).href);
      const full = hasScheme ? raw : baseHref.replace(/\/[^/]*$/, "/") + raw;
      const match = full.match(/^(\w+:)\/\/([^/:?#]+)(:\d+)?(.*)$/);
      this.protocol = match?.[1] || "";
      this.hostname = match?.[2] || "";
      this.port = (match?.[3] || "").slice(1);
      this.pathname = (match?.[4] || "/").split("?")[0].split("#")[0] || "/";
      this.search = full.includes("?") ? "?" + full.split("?")[1].split("#")[0] : "";
      this.hash = full.includes("#") ? "#" + full.split("#")[1] : "";
      this.host = this.hostname + (this.port ? ":" + this.port : "");
      this.href = this.protocol + "//" + this.host + this.pathname + this.search + this.hash;
      this.origin = this.protocol + "//" + this.host;
      this.searchParams = new URLSearchParams(this.search);
      const syncHrefFromSearchParams = () => {
        const query = this.searchParams.toString();
        this.search = query ? `?${query}` : "";
        this.href = this.protocol + "//" + this.host + this.pathname + this.search + this.hash;
      };
      for (const method of ["append", "delete", "set", "sort"]) {
        const original = this.searchParams[method]?.bind(this.searchParams);
        if (!original) {
          continue;
        }
        this.searchParams[method] = (...args) => {
          const result = original(...args);
          syncHrefFromSearchParams();
          return result;
        };
      }
    }
    toString() {
      return this.href;
    }
  };
  var URL2 = class _URL {
    #impl;
    #searchParams;
    constructor(input, base) {
      if (arguments.length < 1) {
        throw createMissingArgsError('The "url" argument must be specified');
      }
      try {
        this.#impl = arguments.length >= 2 ? new NativeURL(toNodeUSVString(input), toNodeUSVString(base)) : new NativeURL(toNodeUSVString(input));
      } catch {
        throw createInvalidUrlError();
      }
    }
    static canParse(input, base) {
      if (arguments.length < 1) {
        throw createMissingArgsError('The "url" argument must be specified');
      }
      try {
        if (arguments.length >= 2) {
          new _URL(input, base);
        } else {
          new _URL(input);
        }
        return true;
      } catch {
        return false;
      }
    }
    static createObjectURL(obj) {
      if (typeof Blob === "undefined" || !(obj instanceof Blob)) {
        throw createNodeTypeError(
          'The "obj" argument must be an instance of Blob. Received ' + (obj === null ? "null" : typeof obj),
          "ERR_INVALID_ARG_TYPE"
        );
      }
      const id = `blob:nodedata:${nextBlobUrlId()}`;
      getBlobUrlStore().set(id, obj);
      return id;
    }
    static revokeObjectURL(url) {
      if (arguments.length < 1) {
        throw createMissingArgsError('The "url" argument must be specified');
      }
      if (typeof url !== "string") {
        return;
      }
      getBlobUrlStore().delete(url);
    }
    get href() {
      if (!(this instanceof _URL)) {
        throw createUrlReceiverTypeError();
      }
      return this.#impl.href;
    }
    set href(value) {
      this.#impl.href = toNodeUSVString(value);
    }
    get origin() {
      return this.#impl.origin;
    }
    get protocol() {
      return this.#impl.protocol;
    }
    set protocol(value) {
      this.#impl.protocol = toNodeUSVString(value);
    }
    get username() {
      return this.#impl.username;
    }
    set username(value) {
      this.#impl.username = toNodeUSVString(value);
    }
    get password() {
      return this.#impl.password;
    }
    set password(value) {
      this.#impl.password = toNodeUSVString(value);
    }
    get host() {
      return this.#impl.host;
    }
    set host(value) {
      this.#impl.host = toNodeUSVString(value);
    }
    get hostname() {
      return this.#impl.hostname;
    }
    set hostname(value) {
      this.#impl.hostname = toNodeUSVString(value);
    }
    get port() {
      return this.#impl.port;
    }
    set port(value) {
      this.#impl.port = toNodeUSVString(value);
    }
    get pathname() {
      return this.#impl.pathname;
    }
    set pathname(value) {
      this.#impl.pathname = toNodeUSVString(value);
    }
    get search() {
      if (!(this instanceof _URL)) {
        throw createUrlReceiverTypeError();
      }
      return this.#impl.search;
    }
    set search(value) {
      this.#impl.search = toNodeUSVString(value);
    }
    get searchParams() {
      if (!this.#searchParams) {
        this.#searchParams = new URLSearchParams({
          [kLinkedSearchParams]: () => this.#impl.searchParams
        });
      }
      return this.#searchParams;
    }
    get hash() {
      return this.#impl.hash;
    }
    set hash(value) {
      this.#impl.hash = toNodeUSVString(value);
    }
    toString() {
      if (!(this instanceof _URL)) {
        throw createUrlReceiverTypeError();
      }
      return this.#impl.href;
    }
    toJSON() {
      if (!(this instanceof _URL)) {
        throw createUrlReceiverTypeError();
      }
      return this.#impl.href;
    }
    [inspectCustomSymbol](depth, options, inspect) {
      const inspectName = this.constructor === _URL ? "URL" : this.constructor.name;
      if (depth < 0) {
        return `${inspectName} {}`;
      }
      const formatValue = typeof inspect === "function" ? (value) => inspect(value, options) : (value) => JSON.stringify(value);
      const lines = [
        `${inspectName} {`,
        `  href: ${formatValue(this.href)},`,
        `  origin: ${formatValue(this.origin)},`,
        `  protocol: ${formatValue(this.protocol)},`,
        `  username: ${formatValue(this.username)},`,
        `  password: ${formatValue(this.password)},`,
        `  host: ${formatValue(this.host)},`,
        `  hostname: ${formatValue(this.hostname)},`,
        `  port: ${formatValue(this.port)},`,
        `  pathname: ${formatValue(this.pathname)},`,
        `  search: ${formatValue(this.search)},`,
        `  searchParams: ${this.searchParams[inspectCustomSymbol](depth - 1, void 0, inspect)},`,
        `  hash: ${formatValue(this.hash)}`
      ];
      if (options?.showHidden) {
        lines[lines.length - 1] += ",";
        lines.push(`  [Symbol(context)]: ${formatUrlContext(this, inspect, options)}`);
      }
      lines.push("}");
      return lines.join("\n");
    }
    get [toStringTagSymbol]() {
      return "URL";
    }
  };
  for (const name of ["toString", "toJSON"]) {
    Object.defineProperty(URL2.prototype, name, {
      value: URL2.prototype[name],
      writable: true,
      configurable: true,
      enumerable: true
    });
  }
  for (const name of [
    "href",
    "protocol",
    "username",
    "password",
    "host",
    "hostname",
    "port",
    "pathname",
    "search",
    "hash",
    "origin",
    "searchParams"
  ]) {
    const descriptor = Object.getOwnPropertyDescriptor(URL2.prototype, name);
    if (!descriptor) {
      continue;
    }
    descriptor.enumerable = true;
    Object.defineProperty(URL2.prototype, name, descriptor);
  }
  Object.defineProperties(URL2.prototype, {
    [inspectCustomSymbol]: {
      value: URL2.prototype[inspectCustomSymbol],
      writable: true,
      configurable: true,
      enumerable: false
    },
    [toStringTagSymbol]: {
      get: Object.getOwnPropertyDescriptor(URL2.prototype, toStringTagSymbol)?.get,
      configurable: true,
      enumerable: false
    }
  });
  for (const name of ["canParse", "createObjectURL", "revokeObjectURL"]) {
    Object.defineProperty(URL2, name, {
      value: URL2[name],
      writable: true,
      configurable: true,
      enumerable: true
    });
  }
  function installWhatwgUrlGlobals(target = globalThis) {
    Object.defineProperty(target, "URL", {
      value: URL2,
      writable: true,
      configurable: true,
      enumerable: false
    });
    Object.defineProperty(target, "URLSearchParams", {
      value: URLSearchParams,
      writable: true,
      configurable: true,
      enumerable: false
    });
  }

  // .agent/recovery/secure-exec/nodejs/src/bridge/events.ts
  var eventsErrorMonitor = Symbol("events.errorMonitor");
  var eventsDefaultMaxListeners = 10;
  function cloneEventListeners(emitter, event) {
    const listeners = emitter._events[event];
    return Array.isArray(listeners) ? listeners.slice() : [];
  }
  function removeEventListenerRecord(emitter, event, listener, onceOnly = false) {
    const listeners = emitter._events[event];
    if (!Array.isArray(listeners) || listeners.length === 0) {
      return emitter;
    }
    const next = listeners.filter(
      (record) => record.listener !== listener || onceOnly && !record.once
    );
    if (next.length === 0) {
      delete emitter._events[event];
    } else {
      emitter._events[event] = next;
    }
    return emitter;
  }
  function emitEventRecords(emitter, event, args) {
    const listeners = cloneEventListeners(emitter, event);
    if (listeners.length === 0) {
      return false;
    }
    for (const record of listeners) {
      if (record.once) {
        removeEventListenerRecord(emitter, event, record.listener, true);
      }
      record.listener.apply(emitter, args);
    }
    return true;
  }
  function topLevelEventListenerCount(emitter, event) {
    return cloneEventListeners(emitter, event).length;
  }
  function topLevelGetEventListeners(emitter, event) {
    return cloneEventListeners(emitter, event).map((record) => record.listener);
  }
  function topLevelGetMaxListeners(emitter) {
    if (emitter && typeof emitter.getMaxListeners === "function") {
      return emitter.getMaxListeners();
    }
    return eventsDefaultMaxListeners;
  }
  function topLevelSetMaxListeners(n, ...emitters) {
    for (const emitter of emitters) {
      if (emitter && typeof emitter.setMaxListeners === "function") {
        emitter.setMaxListeners(n);
      }
    }
  }
  function addAbortListener(signal, listener) {
    if (!signal || typeof signal.addEventListener !== "function") {
      throw new TypeError("AbortSignal is required");
    }
    const wrapped = () => listener();
    if (signal.aborted) {
      queueMicrotask(wrapped);
      return { dispose() {
      } };
    }
    signal.addEventListener("abort", wrapped, { once: true });
    return {
      dispose() {
        signal.removeEventListener("abort", wrapped);
      }
    };
  }
  function once(emitter, eventName) {
    return new Promise((resolve, reject) => {
      const onEvent = (...args) => {
        if (typeof emitter.removeListener === "function") {
          emitter.removeListener("error", onError);
        }
        resolve(args);
      };
      const onError = (error) => {
        if (typeof emitter.removeListener === "function") {
          emitter.removeListener(eventName, onEvent);
        }
        reject(error);
      };
      emitter.once(eventName, onEvent);
      if (eventName !== "error" && typeof emitter.once === "function") {
        emitter.once("error", onError);
      }
    });
  }
  var EventEmitter = class {
    constructor() {
      this._events = Object.create(null);
      this._maxListeners = eventsDefaultMaxListeners;
    }
    addListener(event, listener) {
      return this.on(event, listener);
    }
    on(event, listener) {
      if (typeof listener !== "function") {
        throw new TypeError("listener must be a function");
      }
      const key = String(event);
      const listeners = this._events[key] ?? [];
      listeners.push({ listener, once: false });
      this._events[key] = listeners;
      return this;
    }
    once(event, listener) {
      if (typeof listener !== "function") {
        throw new TypeError("listener must be a function");
      }
      const key = String(event);
      const listeners = this._events[key] ?? [];
      listeners.push({ listener, once: true });
      this._events[key] = listeners;
      return this;
    }
    prependListener(event, listener) {
      if (typeof listener !== "function") {
        throw new TypeError("listener must be a function");
      }
      const key = String(event);
      const listeners = this._events[key] ?? [];
      listeners.unshift({ listener, once: false });
      this._events[key] = listeners;
      return this;
    }
    prependOnceListener(event, listener) {
      if (typeof listener !== "function") {
        throw new TypeError("listener must be a function");
      }
      const key = String(event);
      const listeners = this._events[key] ?? [];
      listeners.unshift({ listener, once: true });
      this._events[key] = listeners;
      return this;
    }
    removeListener(event, listener) {
      return removeEventListenerRecord(this, String(event), listener);
    }
    off(event, listener) {
      return this.removeListener(event, listener);
    }
    removeAllListeners(event) {
      if (typeof event === "undefined") {
        this._events = Object.create(null);
      } else {
        delete this._events[String(event)];
      }
      return this;
    }
    emit(event, ...args) {
      const key = String(event);
      if (key === "error" && topLevelEventListenerCount(this, key) === 0) {
        throw args[0] instanceof Error ? args[0] : new Error(String(args[0] ?? "Unhandled error event"));
      }
      let handled = emitEventRecords(this, key, args);
      if (key === "error") {
        handled = emitEventRecords(this, String(eventsErrorMonitor), args) || handled;
      }
      return handled;
    }
    listeners(event) {
      return topLevelGetEventListeners(this, String(event));
    }
    rawListeners(event) {
      return this.listeners(event);
    }
    listenerCount(event) {
      return topLevelEventListenerCount(this, String(event));
    }
    eventNames() {
      return Object.keys(this._events);
    }
    setMaxListeners(n) {
      this._maxListeners = Number(n);
      return this;
    }
    getMaxListeners() {
      return Number.isFinite(this._maxListeners) ? this._maxListeners : eventsDefaultMaxListeners;
    }
  };
  EventEmitter.once = once;
  EventEmitter.getEventListeners = topLevelGetEventListeners;
  EventEmitter.getMaxListeners = topLevelGetMaxListeners;
  EventEmitter.setMaxListeners = topLevelSetMaxListeners;
  Object.defineProperty(EventEmitter, "defaultMaxListeners", {
    get() {
      return eventsDefaultMaxListeners;
    },
    set(value) {
      eventsDefaultMaxListeners = Number(value);
    }
  });
  var eventsModule = {
    addAbortListener,
    defaultMaxListeners: eventsDefaultMaxListeners,
    errorMonitor: eventsErrorMonitor,
    EventEmitter,
    getEventListeners: topLevelGetEventListeners,
    getMaxListeners: topLevelGetMaxListeners,
    listenerCount: topLevelEventListenerCount,
    once,
    setMaxListeners: topLevelSetMaxListeners
  };
  exposeCustomGlobal("_eventsModule", eventsModule);

  // .agent/recovery/secure-exec/nodejs/src/bridge/process.ts
  var import_buffer2 = __toESM(require_buffer(), 1);
  function readProcessConfig() {
    return {
      platform: typeof _processConfig !== "undefined" && _processConfig.platform || "linux",
      arch: typeof _processConfig !== "undefined" && _processConfig.arch || "x64",
      version: typeof _processConfig !== "undefined" && _processConfig.version || "v22.0.0",
      cwd: typeof _processConfig !== "undefined" && _processConfig.cwd || "/root",
      env: typeof _processConfig !== "undefined" && _processConfig.env || {},
      argv: typeof _processConfig !== "undefined" && _processConfig.argv || [
        "node",
        "script.js"
      ],
      execPath: typeof _processConfig !== "undefined" && _processConfig.execPath || "/usr/bin/node",
      pid: typeof _processConfig !== "undefined" && _processConfig.pid || 1,
      ppid: typeof _processConfig !== "undefined" && _processConfig.ppid || 0,
      uid: typeof _processConfig !== "undefined" && _processConfig.uid || 0,
      gid: typeof _processConfig !== "undefined" && _processConfig.gid || 0,
      stdin: typeof _processConfig !== "undefined" ? _processConfig.stdin : void 0,
      timingMitigation: typeof _processConfig !== "undefined" && _processConfig.timingMitigation || "off",
      frozenTimeMs: typeof _processConfig !== "undefined" ? _processConfig.frozenTimeMs : void 0
    };
  }
  var config2 = readProcessConfig();
  function getNowMs() {
    if (config2.timingMitigation === "freeze" && typeof config2.frozenTimeMs === "number") {
      return config2.frozenTimeMs;
    }
    return typeof performance !== "undefined" && performance.now ? performance.now() : Date.now();
  }
  var _processStartTime = getNowMs();
  var BUFFER_MAX_LENGTH = typeof import_buffer2.Buffer.kMaxLength === "number" ? import_buffer2.Buffer.kMaxLength : 2147483647;
  var BUFFER_MAX_STRING_LENGTH = typeof import_buffer2.Buffer.kStringMaxLength === "number" ? import_buffer2.Buffer.kStringMaxLength : 536870888;
  var BUFFER_CONSTANTS = Object.freeze({
    MAX_LENGTH: BUFFER_MAX_LENGTH,
    MAX_STRING_LENGTH: BUFFER_MAX_STRING_LENGTH
  });
  var bufferPolyfillMutable = import_buffer2.Buffer;
  if (typeof bufferPolyfillMutable.kMaxLength !== "number") {
    bufferPolyfillMutable.kMaxLength = BUFFER_MAX_LENGTH;
  }
  if (typeof bufferPolyfillMutable.kStringMaxLength !== "number") {
    bufferPolyfillMutable.kStringMaxLength = BUFFER_MAX_STRING_LENGTH;
  }
  if (typeof bufferPolyfillMutable.constants !== "object" || bufferPolyfillMutable.constants === null) {
    bufferPolyfillMutable.constants = {
      MAX_LENGTH: BUFFER_MAX_LENGTH,
      MAX_STRING_LENGTH: BUFFER_MAX_STRING_LENGTH
    };
  }
  var bufferProto = import_buffer2.Buffer.prototype;
  if (typeof bufferProto.utf8Slice !== "function") {
    const encodings = ["utf8", "latin1", "ascii", "hex", "base64", "ucs2", "utf16le"];
    for (const enc of encodings) {
      if (typeof bufferProto[enc + "Slice"] !== "function") {
        bufferProto[enc + "Slice"] = function(start, end) {
          return this.toString(enc, start, end);
        };
      }
      if (typeof bufferProto[enc + "Write"] !== "function") {
        bufferProto[enc + "Write"] = function(string, offset, length) {
          return this.write(string, offset ?? 0, length ?? this.length - (offset ?? 0), enc);
        };
      }
    }
  }
  var bufferCtorMutable = import_buffer2.Buffer;
  if (typeof bufferCtorMutable.allocUnsafe === "function" && !bufferCtorMutable.allocUnsafe._secureExecPatched) {
    const originalAllocUnsafe = bufferCtorMutable.allocUnsafe;
    bufferCtorMutable.allocUnsafe = function patchedAllocUnsafe(size) {
      try {
        return originalAllocUnsafe.call(this, size);
      } catch (error) {
        if (error instanceof RangeError && typeof size === "number" && size > BUFFER_MAX_LENGTH) {
          throw new Error("Array buffer allocation failed");
        }
        throw error;
      }
    };
    bufferCtorMutable.allocUnsafe._secureExecPatched = true;
  }
  var _exitCode = 0;
  var _exited = false;
  var ProcessExitError = class extends Error {
    code;
    _isProcessExit;
    constructor(code) {
      super("process.exit(" + code + ")");
      this.name = "ProcessExitError";
      this.code = code;
      this._isProcessExit = true;
    }
  };
  exposeCustomGlobal("ProcessExitError", ProcessExitError);
  var _signalNumbers = {
    SIGHUP: 1,
    SIGINT: 2,
    SIGQUIT: 3,
    SIGILL: 4,
    SIGTRAP: 5,
    SIGABRT: 6,
    SIGBUS: 7,
    SIGFPE: 8,
    SIGKILL: 9,
    SIGUSR1: 10,
    SIGSEGV: 11,
    SIGUSR2: 12,
    SIGPIPE: 13,
    SIGALRM: 14,
    SIGTERM: 15,
    SIGCHLD: 17,
    SIGCONT: 18,
    SIGSTOP: 19,
    SIGTSTP: 20,
    SIGTTIN: 21,
    SIGTTOU: 22,
    SIGURG: 23,
    SIGXCPU: 24,
    SIGXFSZ: 25,
    SIGVTALRM: 26,
    SIGPROF: 27,
    SIGWINCH: 28,
    SIGIO: 29,
    SIGPWR: 30,
    SIGSYS: 31
  };
  var _signalNamesByNumber = Object.fromEntries(
    Object.entries(_signalNumbers).map(([name, num]) => [num, name])
  );
  var _ignoredSelfSignals = /* @__PURE__ */ new Set(["SIGWINCH", "SIGCHLD", "SIGCONT", "SIGURG"]);
  function _resolveSignal(signal) {
    if (signal === void 0 || signal === null) return 15;
    if (typeof signal === "number") return signal;
    const num = _signalNumbers[signal];
    if (num !== void 0) return num;
    throw new Error("Unknown signal: " + signal);
  }
  var _processListeners = {};
  var _processOnceListeners = {};
  var _processMaxListeners = 10;
  var _processMaxListenersWarned = /* @__PURE__ */ new Set();
  function _addListener(event, listener, once = false) {
    const target = once ? _processOnceListeners : _processListeners;
    if (!target[event]) {
      target[event] = [];
    }
    target[event].push(listener);
    if (_processMaxListeners > 0 && !_processMaxListenersWarned.has(event)) {
      const total = (_processListeners[event]?.length ?? 0) + (_processOnceListeners[event]?.length ?? 0);
      if (total > _processMaxListeners) {
        _processMaxListenersWarned.add(event);
        const warning = `MaxListenersExceededWarning: Possible EventEmitter memory leak detected. ${total} ${event} listeners added to [process]. MaxListeners is ${_processMaxListeners}. Use emitter.setMaxListeners() to increase limit`;
        if (typeof _error !== "undefined") {
          _error.applySync(void 0, [warning]);
        }
      }
    }
    return process2;
  }
  function _removeListener(event, listener) {
    if (_processListeners[event]) {
      const idx = _processListeners[event].indexOf(listener);
      if (idx !== -1) _processListeners[event].splice(idx, 1);
    }
    if (_processOnceListeners[event]) {
      const idx = _processOnceListeners[event].indexOf(listener);
      if (idx !== -1) _processOnceListeners[event].splice(idx, 1);
    }
    return process2;
  }
  function _emit(event, ...args) {
    let handled = false;
    if (_processListeners[event]) {
      for (const listener of _processListeners[event]) {
        listener.call(process2, ...args);
        handled = true;
      }
    }
    if (_processOnceListeners[event]) {
      const listeners = _processOnceListeners[event].slice();
      _processOnceListeners[event] = [];
      for (const listener of listeners) {
        listener.call(process2, ...args);
        handled = true;
      }
    }
    return handled;
  }
  function isProcessExitError(error) {
    return Boolean(
      error && typeof error === "object" && (error._isProcessExit === true || error.name === "ProcessExitError")
    );
  }
  function normalizeAsyncError(error) {
    return error instanceof Error ? error : new Error(String(error));
  }
  function routeAsyncCallbackError(error) {
    if (isProcessExitError(error)) {
      return { handled: false, rethrow: error };
    }
    const normalized = normalizeAsyncError(error);
    try {
      if (_emit("uncaughtException", normalized, "uncaughtException")) {
        return { handled: true, rethrow: null };
      }
    } catch (emitError) {
      return { handled: false, rethrow: emitError };
    }
    return { handled: false, rethrow: normalized };
  }
  function scheduleAsyncRethrow(error) {
    setTimeout2(() => {
      throw error;
    }, 0);
  }
  function _getStdinIsTTY() {
    return typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.stdinIsTTY || false;
  }
  function _getStdoutIsTTY() {
    return typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.stdoutIsTTY || false;
  }
  function _getStderrIsTTY() {
    return typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.stderrIsTTY || false;
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
    const decoder = new TextDecoder();
    const stream = {
      write(data, encodingOrCallback, callback) {
        if (data instanceof Uint8Array || typeof import_buffer2.Buffer !== "undefined" && import_buffer2.Buffer.isBuffer(data)) {
          options.write(decoder.decode(data));
        } else {
          options.write(String(data));
        }
        const done = getWriteCallback(encodingOrCallback, callback);
        if (done) {
          _queueMicrotask(() => done(null));
        }
        return true;
      },
      end() {
        return stream;
      },
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
      emit(event, ...args) {
        return emitListeners(listeners, onceListeners, event, args);
      },
      writable: true,
      get isTTY() {
        return options.isTTY();
      },
      get columns() {
        return typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.cols || 80;
      },
      get rows() {
        return typeof __runtimeTtyConfig !== "undefined" && __runtimeTtyConfig.rows || 24;
      }
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
    return args.map((value) => formatConsoleValue(value)).join(" ");
  }
  globalThis.console = {
    log(...args) {
      _stdout.write(formatConsoleArgs(args));
    },
    info(...args) {
      _stdout.write(formatConsoleArgs(args));
    },
    debug(...args) {
      _stdout.write(formatConsoleArgs(args));
    },
    warn(...args) {
      _stderr.write(formatConsoleArgs(args));
    },
    error(...args) {
      _stderr.write(formatConsoleArgs(args));
    },
    dir(value) {
      _stdout.write(formatConsoleArgs([value]));
    },
    trace(...args) {
      const message = formatConsoleArgs(args);
      const error = new Error(message);
      _stderr.write(error.stack || message);
    },
    assert(condition, ...args) {
      if (!condition) {
        const message = args.length > 0 ? formatConsoleArgs(args) : "Assertion failed";
        _stderr.write(message);
      }
    }
  };
  var _stdinListeners = {};
  var _stdinOnceListeners = {};
  var _stdinLiveDecoder = new TextDecoder();
  var STDIN_HANDLE_ID = "process.stdin";
  var _stdinLiveBuffer = "";
  var _stdinLiveStarted = false;
  var _stdinLiveHandleRegistered = false;
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
      listener(value);
    }
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
  function flushLiveStdinBuffer() {
    if (!getStdinFlowMode() || _stdinLiveBuffer.length === 0) return;
    const chunk = _stdinLiveBuffer;
    _stdinLiveBuffer = "";
    const data = _stdin.encoding ? chunk : import_buffer2.Buffer.from(chunk);
    emitStdinListeners("data", data);
  }
  function finishLiveStdin() {
    if (getStdinEnded()) return;
    setStdinEnded(true);
    flushLiveStdinBuffer();
    emitStdinListeners("end");
    emitStdinListeners("close");
    syncLiveStdinHandle(false);
  }
  function _getStreamStdin() {
    return typeof __runtimeStreamStdin !== "undefined" && !!__runtimeStreamStdin;
  }
  function ensureLiveStdinStarted() {
    if (_stdinLiveStarted) return;
    if (!_getStdinIsTTY() && !_getStreamStdin()) return;
    _stdinLiveStarted = true;
    syncLiveStdinHandle(!_stdin.paused);
    if (_getStreamStdin()) {
      return;
    }
    if (typeof _kernelStdinRead === "undefined") return;
    void (async () => {
      try {
        while (!getStdinEnded()) {
          if (typeof _kernelStdinRead === "undefined") {
            break;
          }
          const next = await _kernelStdinRead.apply(void 0, [65536, 100], {
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
    const chunk = typeof payload === "string" ? payload : payload == null ? "" : import_buffer2.Buffer.from(payload).toString("utf8");
    if (!chunk) {
      return;
    }
    _stdinLiveBuffer += chunk;
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
      if ((_getStdinIsTTY() || _getStreamStdin()) && (event === "data" || event === "end" || event === "close")) {
        ensureLiveStdinStarted();
      }
      if (event === "data" && this.paused) {
        this.resume();
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
      if ((_getStdinIsTTY() || _getStreamStdin()) && (event === "data" || event === "end" || event === "close")) {
        ensureLiveStdinStarted();
      }
      if (event === "data" && this.paused) {
        this.resume();
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
      if (_getStdinIsTTY() || _getStreamStdin()) {
        ensureLiveStdinStarted();
        syncLiveStdinHandle(true);
      }
      this.paused = false;
      setStdinFlowMode(true);
      flushLiveStdinBuffer();
      _emitStdinData();
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
    // For readline compatibility
    [Symbol.asyncIterator]: async function* () {
      const lines = getStdinData().split("\n");
      for (const line of lines) {
        if (line) yield line;
      }
    }
  };
  exposeCustomGlobal("_stdinDispatch", stdinDispatch);
  function hrtime(prev) {
    const now = getNowMs();
    const seconds = Math.floor(now / 1e3);
    const nanoseconds = Math.floor(now % 1e3 * 1e6);
    if (prev) {
      let diffSec = seconds - prev[0];
      let diffNano = nanoseconds - prev[1];
      if (diffNano < 0) {
        diffSec -= 1;
        diffNano += 1e9;
      }
      return [diffSec, diffNano];
    }
    return [seconds, nanoseconds];
  }
  hrtime.bigint = function() {
    const now = getNowMs();
    return BigInt(Math.floor(now * 1e6));
  };
  var _cwd = config2.cwd;
  var _umask = 18;
  var process2 = {
    // Static properties
    platform: config2.platform,
    arch: config2.arch,
    version: config2.version,
    versions: {
      node: config2.version.replace(/^v/, ""),
      v8: "11.3.244.8",
      uv: "1.44.2",
      zlib: "1.2.13",
      brotli: "1.0.9",
      ares: "1.19.0",
      modules: "108",
      nghttp2: "1.52.0",
      napi: "8",
      llhttp: "8.1.0",
      openssl: "3.0.8",
      cldr: "42.0",
      icu: "72.1",
      tz: "2022g",
      unicode: "15.0"
    },
    pid: config2.pid,
    ppid: config2.ppid,
    execPath: config2.execPath,
    execArgv: [],
    argv: config2.argv,
    argv0: config2.argv[0] || "node",
    title: "node",
    env: config2.env,
    // Config stubs
    config: {
      target_defaults: {
        cflags: [],
        default_configuration: "Release",
        defines: [],
        include_dirs: [],
        libraries: []
      },
      variables: {
        node_prefix: "/usr",
        node_shared_libuv: false
      }
    },
    release: {
      name: "node",
      sourceUrl: "https://nodejs.org/download/release/v20.0.0/node-v20.0.0.tar.gz",
      headersUrl: "https://nodejs.org/download/release/v20.0.0/node-v20.0.0-headers.tar.gz"
    },
    // Feature flags
    features: {
      inspector: false,
      debug: false,
      uv: true,
      ipv6: true,
      tls_alpn: true,
      tls_sni: true,
      tls_ocsp: true,
      tls: true
    },
    // Methods
    cwd() {
      return _cwd;
    },
    chdir(dir) {
      let statJson;
      try {
        statJson = _fs.stat.applySyncPromise(void 0, [dir]);
      } catch {
        const err = new Error(`ENOENT: no such file or directory, chdir '${dir}'`);
        err.code = "ENOENT";
        err.errno = -2;
        err.syscall = "chdir";
        err.path = dir;
        throw err;
      }
      const parsed = decodeBridgeJson(statJson);
      if (!parsed.isDirectory) {
        const err = new Error(`ENOTDIR: not a directory, chdir '${dir}'`);
        err.code = "ENOTDIR";
        err.errno = -20;
        err.syscall = "chdir";
        err.path = dir;
        throw err;
      }
      _cwd = dir;
    },
    get exitCode() {
      return _exitCode;
    },
    set exitCode(code) {
      _exitCode = code ?? 0;
    },
    exit(code) {
      const exitCode = code !== void 0 ? code : _exitCode;
      _exitCode = exitCode;
      _exited = true;
      try {
        _emit("exit", exitCode);
      } catch (_e) {
      }
      throw new ProcessExitError(exitCode);
    },
    abort() {
      return process2.exit(1);
    },
    nextTick(callback, ...args) {
      _nextTickQueue.push({ callback, args });
      scheduleNextTickFlush();
    },
    hrtime,
    getuid() {
      return config2.uid;
    },
    getgid() {
      return config2.gid;
    },
    geteuid() {
      return config2.uid;
    },
    getegid() {
      return config2.gid;
    },
    getgroups() {
      return [config2.gid];
    },
    setuid() {
    },
    setgid() {
    },
    seteuid() {
    },
    setegid() {
    },
    setgroups() {
    },
    umask(mask) {
      const oldMask = _umask;
      if (mask !== void 0) {
        _umask = mask;
      }
      return oldMask;
    },
    uptime() {
      return (getNowMs() - _processStartTime) / 1e3;
    },
    memoryUsage() {
      return {
        rss: 50 * 1024 * 1024,
        heapTotal: 20 * 1024 * 1024,
        heapUsed: 10 * 1024 * 1024,
        external: 1 * 1024 * 1024,
        arrayBuffers: 500 * 1024
      };
    },
    cpuUsage(prev) {
      const usage = {
        user: 1e6,
        system: 5e5
      };
      if (prev) {
        return {
          user: usage.user - prev.user,
          system: usage.system - prev.system
        };
      }
      return usage;
    },
    resourceUsage() {
      return {
        userCPUTime: 1e6,
        systemCPUTime: 5e5,
        maxRSS: 50 * 1024,
        sharedMemorySize: 0,
        unsharedDataSize: 0,
        unsharedStackSize: 0,
        minorPageFault: 0,
        majorPageFault: 0,
        swappedOut: 0,
        fsRead: 0,
        fsWrite: 0,
        ipcSent: 0,
        ipcReceived: 0,
        signalsCount: 0,
        voluntaryContextSwitches: 0,
        involuntaryContextSwitches: 0
      };
    },
    kill(pid, signal) {
      if (pid !== process2.pid) {
        const err = new Error("Operation not permitted");
        err.code = "EPERM";
        err.errno = -1;
        err.syscall = "kill";
        throw err;
      }
      const sigNum = _resolveSignal(signal);
      if (sigNum === 0) {
        return true;
      }
      const sigName = _signalNamesByNumber[sigNum] ?? `SIG${sigNum}`;
      if (_emit(sigName, sigName)) {
        return true;
      }
      if (_ignoredSelfSignals.has(sigName)) {
        return true;
      }
      return process2.exit(128 + sigNum);
    },
    // EventEmitter methods
    on(event, listener) {
      return _addListener(event, listener);
    },
    once(event, listener) {
      return _addListener(event, listener, true);
    },
    removeListener(event, listener) {
      return _removeListener(event, listener);
    },
    // off is an alias for removeListener (assigned below to be same reference)
    off: null,
    removeAllListeners(event) {
      if (event) {
        delete _processListeners[event];
        delete _processOnceListeners[event];
      } else {
        Object.keys(_processListeners).forEach((k) => delete _processListeners[k]);
        Object.keys(_processOnceListeners).forEach(
          (k) => delete _processOnceListeners[k]
        );
      }
      return process2;
    },
    addListener(event, listener) {
      return _addListener(event, listener);
    },
    emit(event, ...args) {
      return _emit(event, ...args);
    },
    listeners(event) {
      return [
        ..._processListeners[event] || [],
        ..._processOnceListeners[event] || []
      ];
    },
    listenerCount(event) {
      return (_processListeners[event] || []).length + (_processOnceListeners[event] || []).length;
    },
    prependListener(event, listener) {
      if (!_processListeners[event]) {
        _processListeners[event] = [];
      }
      _processListeners[event].unshift(listener);
      return process2;
    },
    prependOnceListener(event, listener) {
      if (!_processOnceListeners[event]) {
        _processOnceListeners[event] = [];
      }
      _processOnceListeners[event].unshift(listener);
      return process2;
    },
    eventNames() {
      return [
        .../* @__PURE__ */ new Set([
          ...Object.keys(_processListeners),
          ...Object.keys(_processOnceListeners)
        ])
      ];
    },
    setMaxListeners(n) {
      _processMaxListeners = n;
      return process2;
    },
    getMaxListeners() {
      return _processMaxListeners;
    },
    rawListeners(event) {
      return process2.listeners(event);
    },
    // Stdio streams
    stdout: _stdout,
    stderr: _stderr,
    stdin: _stdin,
    // Process state
    connected: false,
    // Module info (will be set by createRequire)
    mainModule: void 0,
    // No-op methods for compatibility
    emitWarning(warning) {
      const msg = typeof warning === "string" ? warning : warning.message;
      _emit("warning", { message: msg, name: "Warning" });
    },
    binding(_name) {
      const error = new Error("process.binding is not supported in sandbox");
      error.code = "ERR_ACCESS_DENIED";
      throw error;
    },
    _linkedBinding(_name) {
      const error = new Error("process._linkedBinding is not supported in sandbox");
      error.code = "ERR_ACCESS_DENIED";
      throw error;
    },
    dlopen() {
      throw new Error("process.dlopen is not supported");
    },
    hasUncaughtExceptionCaptureCallback() {
      return false;
    },
    setUncaughtExceptionCaptureCallback() {
    },
    // Send for IPC (no-op)
    send() {
      return false;
    },
    disconnect() {
    },
    // Report
    report: {
      directory: "",
      filename: "",
      compact: false,
      signal: "SIGUSR2",
      reportOnFatalError: false,
      reportOnSignal: false,
      reportOnUncaughtException: false,
      getReport() {
        return {};
      },
      writeReport() {
        return "";
      }
    },
    // Debug port
    debugPort: 9229,
    // Internal state
    _cwd: config2.cwd,
    _umask: 18
  };
  function applyProcessConfig(nextConfig) {
    syncLiveStdinHandle(false);
    _stdinLiveBuffer = "";
    _stdinLiveStarted = false;
    _stdinLiveDecoder = new TextDecoder();
    for (const key of Object.keys(_stdinListeners)) {
      _stdinListeners[key] = [];
    }
    for (const key of Object.keys(_stdinOnceListeners)) {
      _stdinOnceListeners[key] = [];
    }
    setStdinDataValue(nextConfig.stdin ?? "");
    setStdinPosition(0);
    setStdinEnded(false);
    setStdinFlowMode(false);
    config2 = nextConfig;
    _cwd = nextConfig.cwd;
    process2.platform = nextConfig.platform;
    process2.arch = nextConfig.arch;
    process2.version = nextConfig.version;
    process2.pid = nextConfig.pid;
    process2.ppid = nextConfig.ppid;
    process2.execPath = nextConfig.execPath;
    process2.argv = nextConfig.argv;
    process2.argv0 = nextConfig.argv[0] || "node";
    process2.env = nextConfig.env;
    process2._cwd = nextConfig.cwd;
    process2.stdin.paused = true;
    process2.stdin.encoding = null;
    process2.stdin.isRaw = false;
    process2.versions.node = nextConfig.version.replace(/^v/, "");
  }
  exposeCustomGlobal("__runtimeRefreshProcessConfig", () => {
    applyProcessConfig(readProcessConfig());
  });
  process2.off = process2.removeListener;
  process2.memoryUsage.rss = function() {
    return 50 * 1024 * 1024;
  };
  Object.defineProperty(process2, Symbol.toStringTag, {
    value: "process",
    writable: false,
    configurable: true,
    enumerable: false
  });
  var process_default = process2;
  function ttyIsatty(fd) {
    if (fd === 0) {
      return !!process_default.stdin?.isTTY;
    }
    if (fd === 1) {
      return !!process_default.stdout?.isTTY;
    }
    if (fd === 2) {
      return !!process_default.stderr?.isTTY;
    }
    return false;
  }
  function TtyReadStream(fd) {
    return fd === 0 ? process_default.stdin : void 0;
  }
  function TtyWriteStream(fd) {
    if (fd === 1) {
      return process_default.stdout;
    }
    if (fd === 2) {
      return process_default.stderr;
    }
    return void 0;
  }
  var builtinTtyModule = {
    ReadStream: TtyReadStream,
    WriteStream: TtyWriteStream,
    isatty: ttyIsatty
  };
  var builtinPerformance = typeof performance !== "undefined" && performance && typeof performance.now === "function" ? performance : {
    now() {
      return getNowMs();
    },
    timeOrigin: Date.now() - getNowMs()
  };
  var builtinPerfHooksModule = {
    PerformanceObserver: class {
      observe() {
      }
      disconnect() {
      }
      takeRecords() {
        return [];
      }
    },
    constants: {},
    createHistogram() {
      return {
        percentile() {
          return 0;
        },
        record() {
        }
      };
    },
    performance: builtinPerformance
  };
  var builtinVmModule = {
    Script: class {
      constructor(code, options = {}) {
        this.code = String(code ?? "");
        this.options = options;
      }
      runInThisContext() {
        return builtinVmModule.runInThisContext(this.code, this.options);
      }
    },
    createContext(context = {}) {
      return context;
    },
    isContext(value) {
      return !!value && typeof value === "object";
    },
    runInThisContext(code, _options = {}) {
      return (0, eval)(String(code ?? ""));
    }
  };
  var TIMER_DISPATCH = {
    create: "kernelTimerCreate",
    arm: "kernelTimerArm",
    clear: "kernelTimerClear"
  };
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
    constructor(id) {
      this._id = id;
      this._destroyed = false;
    }
    ref() {
      return this;
    }
    unref() {
      return this;
    }
    hasRef() {
      return true;
    }
    refresh() {
      return this;
    }
    [Symbol.toPrimitive]() {
      return this._id;
    }
  };
  var _timerEntries = /* @__PURE__ */ new Map();
  var _timerDrainResolvers = [];
  function checkTimerDrain() {
    if (_timerEntries.size === 0 && _timerDrainResolvers.length > 0) {
      const resolvers = _timerDrainResolvers;
      _timerDrainResolvers = [];
      resolvers.forEach((r) => r());
    }
  }
  function _getPendingTimerCount() {
    return _timerEntries.size;
  }
  function _waitForTimerDrain() {
    if (_timerEntries.size === 0) return Promise.resolve();
    return new Promise((resolve) => {
      _timerDrainResolvers.push(resolve);
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
    _queueMicrotask(flushNextTickQueue);
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
    if (entry.repeat && _timerEntries.has(timerId)) {
      armKernelTimer(timerId);
    }
    checkTimerDrain();
  }
  function setTimeout2(callback, delay, ...args) {
    const actualDelay = normalizeTimerDelay(delay);
    const id = createKernelTimer(actualDelay, false);
    const handle = new TimerHandle(id);
    _timerEntries.set(id, {
      handle,
      callback,
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
    _timerEntries.set(id, {
      handle,
      callback,
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
  exposeCustomGlobal("_getPendingTimerCount", _getPendingTimerCount);
  exposeCustomGlobal("_waitForTimerDrain", _waitForTimerDrain);
  function setImmediate(callback, ...args) {
    return setTimeout2(callback, 0, ...args);
  }
  function clearImmediate(id) {
    clearTimeout2(id);
  }
  var Buffer3 = import_buffer2.Buffer;
  function throwUnsupportedCryptoApi(api) {
    throw new Error(`crypto.${api} is not supported in sandbox`);
  }
  var kCryptoKeyToken = /* @__PURE__ */ Symbol("secureExecCryptoKey");
  var kCryptoToken = /* @__PURE__ */ Symbol("secureExecCrypto");
  var kSubtleToken = /* @__PURE__ */ Symbol("secureExecSubtle");
  var ERR_INVALID_THIS2 = "ERR_INVALID_THIS";
  var ERR_ILLEGAL_CONSTRUCTOR = "ERR_ILLEGAL_CONSTRUCTOR";
  function createNodeTypeError2(message, code) {
    const error = new TypeError(message);
    error.code = code;
    return error;
  }
  function createDomLikeError(name, code, message) {
    const error = new Error(message);
    error.name = name;
    error.code = code;
    return error;
  }
  function assertCryptoReceiver(receiver) {
    if (!(receiver instanceof SandboxCrypto) || receiver._token !== kCryptoToken) {
      throw createNodeTypeError2('Value of "this" must be of type Crypto', ERR_INVALID_THIS2);
    }
  }
  function assertSubtleReceiver(receiver) {
    if (!(receiver instanceof SandboxSubtleCrypto) || receiver._token !== kSubtleToken) {
      throw createNodeTypeError2('Value of "this" must be of type SubtleCrypto', ERR_INVALID_THIS2);
    }
  }
  function isIntegerTypedArray(value) {
    if (!ArrayBuffer.isView(value) || value instanceof DataView) {
      return false;
    }
    return value instanceof Int8Array || value instanceof Int16Array || value instanceof Int32Array || value instanceof Uint8Array || value instanceof Uint16Array || value instanceof Uint32Array || value instanceof Uint8ClampedArray || value instanceof BigInt64Array || value instanceof BigUint64Array || import_buffer2.Buffer.isBuffer(value);
  }
  function toBase64(data) {
    if (typeof data === "string") {
      return import_buffer2.Buffer.from(data).toString("base64");
    }
    if (data instanceof ArrayBuffer) {
      return import_buffer2.Buffer.from(new Uint8Array(data)).toString("base64");
    }
    if (ArrayBuffer.isView(data)) {
      return import_buffer2.Buffer.from(
        new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
      ).toString("base64");
    }
    return import_buffer2.Buffer.from(data).toString("base64");
  }
  function toArrayBuffer(data) {
    const buf = import_buffer2.Buffer.from(data, "base64");
    return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
  }
  function normalizeAlgorithm(algorithm) {
    if (typeof algorithm === "string") {
      return { name: algorithm };
    }
    return algorithm ?? {};
  }
  function normalizeBridgeAlgorithm(algorithm) {
    const normalized = { ...normalizeAlgorithm(algorithm) };
    const hash = normalized.hash;
    const publicExponent = normalized.publicExponent;
    const iv = normalized.iv;
    const additionalData = normalized.additionalData;
    const salt = normalized.salt;
    const info = normalized.info;
    const context = normalized.context;
    const label = normalized.label;
    const publicKey = normalized.public;
    if (hash) {
      normalized.hash = normalizeAlgorithm(hash);
    }
    if (publicExponent && ArrayBuffer.isView(publicExponent)) {
      normalized.publicExponent = import_buffer2.Buffer.from(
        new Uint8Array(
          publicExponent.buffer,
          publicExponent.byteOffset,
          publicExponent.byteLength
        )
      ).toString("base64");
    }
    if (iv) {
      normalized.iv = toBase64(iv);
    }
    if (additionalData) {
      normalized.additionalData = toBase64(additionalData);
    }
    if (salt) {
      normalized.salt = toBase64(salt);
    }
    if (info) {
      normalized.info = toBase64(info);
    }
    if (context) {
      normalized.context = toBase64(context);
    }
    if (label) {
      normalized.label = toBase64(label);
    }
    if (publicKey && typeof publicKey === "object" && "_keyData" in publicKey) {
      normalized.public = publicKey._keyData;
    }
    return normalized;
  }
  var SandboxCryptoKey = class {
    type;
    extractable;
    algorithm;
    usages;
    _keyData;
    _pem;
    _jwk;
    _raw;
    _sourceKeyObjectData;
    [kCryptoKeyToken];
    constructor(keyData, token) {
      if (token !== kCryptoKeyToken || !keyData) {
        throw createNodeTypeError2("Illegal constructor", ERR_ILLEGAL_CONSTRUCTOR);
      }
      this.type = keyData.type;
      this.extractable = keyData.extractable;
      this.algorithm = keyData.algorithm;
      this.usages = keyData.usages;
      this._keyData = keyData;
      this._pem = keyData._pem;
      this._jwk = keyData._jwk;
      this._raw = keyData._raw;
      this._sourceKeyObjectData = keyData._sourceKeyObjectData;
      this[kCryptoKeyToken] = true;
    }
  };
  Object.defineProperty(SandboxCryptoKey.prototype, Symbol.toStringTag, {
    value: "CryptoKey",
    configurable: true
  });
  Object.defineProperty(SandboxCryptoKey, Symbol.hasInstance, {
    value(candidate) {
      return Boolean(
        candidate && typeof candidate === "object" && (candidate[kCryptoKeyToken] === true || "_keyData" in candidate && candidate[Symbol.toStringTag] === "CryptoKey")
      );
    },
    configurable: true
  });
  function createCryptoKey(keyData) {
    const globalCryptoKey = globalThis.CryptoKey;
    if (typeof globalCryptoKey === "function" && globalCryptoKey.prototype && globalCryptoKey.prototype !== SandboxCryptoKey.prototype) {
      const key = Object.create(globalCryptoKey.prototype);
      key.type = keyData.type;
      key.extractable = keyData.extractable;
      key.algorithm = keyData.algorithm;
      key.usages = keyData.usages;
      key._keyData = keyData;
      key._pem = keyData._pem;
      key._jwk = keyData._jwk;
      key._raw = keyData._raw;
      key._sourceKeyObjectData = keyData._sourceKeyObjectData;
      return key;
    }
    return new SandboxCryptoKey(keyData, kCryptoKeyToken);
  }
  function subtleCall(request) {
    if (typeof _cryptoSubtle === "undefined") {
      throw new Error("crypto.subtle is not supported in sandbox");
    }
    return _cryptoSubtle.applySync(void 0, [JSON.stringify(request)]);
  }
  var SandboxSubtleCrypto = class {
    _token;
    constructor(token) {
      if (token !== kSubtleToken) {
        throw createNodeTypeError2("Illegal constructor", ERR_ILLEGAL_CONSTRUCTOR);
      }
      this._token = token;
    }
    digest(algorithm, data) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "digest",
            algorithm: normalizeAlgorithm(algorithm).name,
            data: toBase64(data)
          })
        );
        return toArrayBuffer(result.data);
      });
    }
    generateKey(algorithm, extractable, keyUsages) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "generateKey",
            algorithm: normalizeBridgeAlgorithm(algorithm),
            extractable,
            usages: Array.from(keyUsages)
          })
        );
        if ("publicKey" in result && "privateKey" in result) {
          return {
            publicKey: createCryptoKey(result.publicKey),
            privateKey: createCryptoKey(result.privateKey)
          };
        }
        return createCryptoKey(result.key);
      });
    }
    importKey(format, keyData, algorithm, extractable, keyUsages) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "importKey",
            format,
            keyData: format === "jwk" ? keyData : toBase64(keyData),
            algorithm: normalizeBridgeAlgorithm(algorithm),
            extractable,
            usages: Array.from(keyUsages)
          })
        );
        return createCryptoKey(result.key);
      });
    }
    exportKey(format, key) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "exportKey",
            format,
            key: key._keyData
          })
        );
        if (format === "jwk") {
          return result.jwk;
        }
        return toArrayBuffer(result.data ?? "");
      });
    }
    encrypt(algorithm, key, data) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "encrypt",
            algorithm: normalizeBridgeAlgorithm(algorithm),
            key: key._keyData,
            data: toBase64(data)
          })
        );
        return toArrayBuffer(result.data);
      });
    }
    decrypt(algorithm, key, data) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "decrypt",
            algorithm: normalizeBridgeAlgorithm(algorithm),
            key: key._keyData,
            data: toBase64(data)
          })
        );
        return toArrayBuffer(result.data);
      });
    }
    sign(algorithm, key, data) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "sign",
            algorithm: normalizeBridgeAlgorithm(algorithm),
            key: key._keyData,
            data: toBase64(data)
          })
        );
        return toArrayBuffer(result.data);
      });
    }
    verify(algorithm, key, signature, data) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "verify",
            algorithm: normalizeBridgeAlgorithm(algorithm),
            key: key._keyData,
            signature: toBase64(signature),
            data: toBase64(data)
          })
        );
        return result.result;
      });
    }
    deriveBits(algorithm, baseKey, length) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "deriveBits",
            algorithm: normalizeBridgeAlgorithm(algorithm),
            baseKey: baseKey._keyData,
            length
          })
        );
        return toArrayBuffer(result.data);
      });
    }
    deriveKey(algorithm, baseKey, derivedKeyAlgorithm, extractable, keyUsages) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "deriveKey",
            algorithm: normalizeBridgeAlgorithm(algorithm),
            baseKey: baseKey._keyData,
            derivedKeyAlgorithm: normalizeBridgeAlgorithm(derivedKeyAlgorithm),
            extractable,
            usages: Array.from(keyUsages)
          })
        );
        return createCryptoKey(result.key);
      });
    }
    wrapKey(format, key, wrappingKey, wrapAlgorithm) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "wrapKey",
            format,
            key: key._keyData,
            wrappingKey: wrappingKey._keyData,
            wrapAlgorithm: normalizeBridgeAlgorithm(wrapAlgorithm)
          })
        );
        return toArrayBuffer(result.data);
      });
    }
    unwrapKey(format, wrappedKey, unwrappingKey, unwrapAlgorithm, unwrappedKeyAlgorithm, extractable, keyUsages) {
      assertSubtleReceiver(this);
      return Promise.resolve().then(() => {
        const result = JSON.parse(
          subtleCall({
            op: "unwrapKey",
            format,
            wrappedKey: toBase64(wrappedKey),
            unwrappingKey: unwrappingKey._keyData,
            unwrapAlgorithm: normalizeBridgeAlgorithm(unwrapAlgorithm),
            unwrappedKeyAlgorithm: normalizeBridgeAlgorithm(unwrappedKeyAlgorithm),
            extractable,
            usages: Array.from(keyUsages)
          })
        );
        return createCryptoKey(result.key);
      });
    }
  };
  var subtleCrypto = new SandboxSubtleCrypto(kSubtleToken);
  var SandboxCrypto = class {
    _token;
    constructor(token) {
      if (token !== kCryptoToken) {
        throw createNodeTypeError2("Illegal constructor", ERR_ILLEGAL_CONSTRUCTOR);
      }
      this._token = token;
    }
    get subtle() {
      assertCryptoReceiver(this);
      return subtleCrypto;
    }
    getRandomValues(array) {
      assertCryptoReceiver(this);
      if (!isIntegerTypedArray(array)) {
        throw createDomLikeError(
          "TypeMismatchError",
          17,
          "The data argument must be an integer-type TypedArray"
        );
      }
      if (typeof _cryptoRandomFill === "undefined") {
        throwUnsupportedCryptoApi("getRandomValues");
      }
      if (array.byteLength > 65536) {
        throw createDomLikeError(
          "QuotaExceededError",
          22,
          `The ArrayBufferView's byte length (${array.byteLength}) exceeds the number of bytes of entropy available via this API (65536)`
        );
      }
      const bytes = new Uint8Array(array.buffer, array.byteOffset, array.byteLength);
      try {
        const base64 = _cryptoRandomFill.applySync(void 0, [bytes.byteLength]);
        const hostBytes = import_buffer2.Buffer.from(base64, "base64");
        if (hostBytes.byteLength !== bytes.byteLength) {
          throw new Error("invalid host entropy size");
        }
        bytes.set(hostBytes);
        return array;
      } catch {
        throwUnsupportedCryptoApi("getRandomValues");
      }
    }
    randomUUID() {
      assertCryptoReceiver(this);
      if (typeof _cryptoRandomUUID === "undefined") {
        throwUnsupportedCryptoApi("randomUUID");
      }
      try {
        const uuid = _cryptoRandomUUID.applySync(void 0, []);
        if (typeof uuid !== "string") {
          throw new Error("invalid host uuid");
        }
        return uuid;
      } catch {
        throwUnsupportedCryptoApi("randomUUID");
      }
    }
  };
  var cryptoPolyfillInstance = new SandboxCrypto(kCryptoToken);
  var cryptoPolyfill = cryptoPolyfillInstance;
  function createBuiltinHash(algorithm) {
    const chunks = [];
    return {
      update(data, encoding) {
        const buffer = typeof data === "string" ? import_buffer2.Buffer.from(data, encoding || "utf8") : import_buffer2.Buffer.from(data);
        chunks.push(buffer);
        return this;
      },
      digest(encoding) {
        if (typeof _cryptoHashDigest === "undefined") {
          throwUnsupportedCryptoApi("createHash");
        }
        const input = chunks.length === 1 ? chunks[0] : import_buffer2.Buffer.concat(chunks);
        const resultBase64 = _cryptoHashDigest.applySync(void 0, [
          String(algorithm),
          input.toString("base64")
        ]);
        const result = import_buffer2.Buffer.from(String(resultBase64 || ""), "base64");
        return encoding ? result.toString(encoding) : result;
      }
    };
  }
  function createBuiltinHmac(algorithm, key) {
    const chunks = [];
    const keyBuffer = typeof key === "string" ? import_buffer2.Buffer.from(key) : import_buffer2.Buffer.from(key ?? []);
    return {
      update(data, encoding) {
        const buffer = typeof data === "string" ? import_buffer2.Buffer.from(data, encoding || "utf8") : import_buffer2.Buffer.from(data);
        chunks.push(buffer);
        return this;
      },
      digest(encoding) {
        if (typeof _cryptoHmacDigest === "undefined") {
          throwUnsupportedCryptoApi("createHmac");
        }
        const input = chunks.length === 1 ? chunks[0] : import_buffer2.Buffer.concat(chunks);
        const resultBase64 = _cryptoHmacDigest.applySync(void 0, [
          String(algorithm),
          keyBuffer.toString("base64"),
          input.toString("base64")
        ]);
        const result = import_buffer2.Buffer.from(String(resultBase64 || ""), "base64");
        return encoding ? result.toString(encoding) : result;
      }
    };
  }
  var builtinCryptoModule = {
    createHash(algorithm) {
      return createBuiltinHash(algorithm);
    },
    createHmac(algorithm, key) {
      return createBuiltinHmac(algorithm, key);
    },
    getFips() {
      return 0;
    },
    getRandomValues(array) {
      return cryptoPolyfill.getRandomValues(array);
    },
    randomBytes(size) {
      const length = Math.max(0, Number(size) || 0);
      const bytes = new Uint8Array(length);
      cryptoPolyfill.getRandomValues(bytes);
      return import_buffer2.Buffer.from(bytes);
    },
    randomUUID() {
      return cryptoPolyfill.randomUUID();
    },
    subtle: subtleCrypto,
    webcrypto: cryptoPolyfill
  };
  function setupGlobals() {
    const g = globalThis;
    g.process = process2;
    g.setTimeout = setTimeout2;
    g.clearTimeout = clearTimeout2;
    g.setInterval = setInterval;
    g.clearInterval = clearInterval;
    g.setImmediate = setImmediate;
    g.clearImmediate = clearImmediate;
    if (typeof g.queueMicrotask === "undefined") {
      g.queueMicrotask = _queueMicrotask;
    }
    installWhatwgUrlGlobals(g);
    g.TextEncoder = TextEncoder2;
    g.TextDecoder = TextDecoder;
    g.Event = Event;
    g.CustomEvent = CustomEvent;
    g.EventTarget = EventTarget;
    if (typeof g.Buffer === "undefined") {
      g.Buffer = Buffer3;
    }
    const globalBuffer = g.Buffer;
    if (typeof globalBuffer.kMaxLength !== "number") {
      globalBuffer.kMaxLength = BUFFER_MAX_LENGTH;
    }
    if (typeof globalBuffer.kStringMaxLength !== "number") {
      globalBuffer.kStringMaxLength = BUFFER_MAX_STRING_LENGTH;
    }
    if (typeof globalBuffer.constants !== "object" || globalBuffer.constants === null) {
      globalBuffer.constants = BUFFER_CONSTANTS;
    }
    const builtinUtilModule = globalThis.__agentOsBuiltinUtilModule;
    if (builtinUtilModule?.types) {
      builtinUtilModule.types.isProxy = () => false;
    }
    if (typeof g.atob === "undefined" || typeof g.btoa === "undefined") {
      const base64 = require_base64_js();
      if (typeof g.atob === "undefined") {
        g.atob = (value) => {
          const bytes = base64.toByteArray(String(value));
          let decoded = "";
          for (const byte of bytes) {
            decoded += String.fromCharCode(byte);
          }
          return decoded;
        };
      }
      if (typeof g.btoa === "undefined") {
        g.btoa = (value) => {
          const input = String(value);
          const bytes = new Uint8Array(input.length);
          for (let index = 0; index < input.length; index += 1) {
            const code = input.charCodeAt(index);
            if (code > 255) {
              throw new TypeError("Invalid character");
            }
            bytes[index] = code;
          }
          return base64.fromByteArray(bytes);
        };
      }
    }
    if (typeof g.Crypto === "undefined") {
      g.Crypto = SandboxCrypto;
    }
    if (typeof g.SubtleCrypto === "undefined") {
      g.SubtleCrypto = SandboxSubtleCrypto;
    }
    if (typeof g.CryptoKey === "undefined") {
      g.CryptoKey = SandboxCryptoKey;
    }
    if (typeof g.crypto === "undefined") {
      g.crypto = cryptoPolyfill;
    } else {
      const cryptoObj = g.crypto;
      if (typeof cryptoObj.getRandomValues === "undefined") {
        cryptoObj.getRandomValues = cryptoPolyfill.getRandomValues;
      }
      if (typeof cryptoObj.randomUUID === "undefined") {
        cryptoObj.randomUUID = cryptoPolyfill.randomUUID;
      }
      if (typeof cryptoObj.subtle === "undefined") {
        cryptoObj.subtle = cryptoPolyfill.subtle;
      }
    }
    g.fetch = fetch;
    g.Headers = UndiciHeaders;
    g.Request = UndiciRequest;
    g.Response = UndiciResponse;
  }

  // .agent/recovery/secure-exec/nodejs/src/bridge/module.ts
  var initialModuleCache = {};
  var initialPendingModules = {};
  var initialCurrentModule = {
    id: "/<entry>.js",
    filename: "/<entry>.js",
    dirname: "/",
    exports: {},
    loaded: false
  };
  exposeMutableRuntimeStateGlobal("_moduleCache", initialModuleCache);
  exposeMutableRuntimeStateGlobal("_pendingModules", initialPendingModules);
  exposeMutableRuntimeStateGlobal("_currentModule", initialCurrentModule);
  function _pathDirname(p) {
    const lastSlash = p.lastIndexOf("/");
    if (lastSlash === -1) return ".";
    if (lastSlash === 0) return "/";
    return p.slice(0, lastSlash);
  }
  function _parseFileUrl(url) {
    if (url.startsWith("file://")) {
      let path = url.slice(7);
      if (path.startsWith("/")) {
        return path;
      }
      return "/" + path;
    }
    return url;
  }
  function createRequire(filename) {
    if (typeof filename !== "string" && !(filename instanceof URL)) {
      throw new TypeError("filename must be a string or URL");
    }
    const filepath = _parseFileUrl(String(filename));
    const dirname = _pathDirname(filepath);
    const builtins = [
      "fs",
      "path",
      "os",
      "events",
      "util",
      "http",
      "_http_common",
      "https",
      "dns",
      "dgram",
      "child_process",
      "stream",
      "buffer",
      "url",
      "querystring",
      "crypto",
      "sqlite",
      "zlib",
      "assert",
      "tty",
      "net",
      "tls"
    ];
    const resolvePaths = function(request) {
      if (builtins.includes(request) || request.startsWith("node:")) {
        return null;
      }
      if (request.startsWith("./") || request.startsWith("../") || request.startsWith("/")) {
        return [dirname];
      }
      const paths = [];
      let current = dirname;
      while (current !== "/") {
        paths.push(current + "/node_modules");
        current = _pathDirname(current);
      }
      paths.push("/node_modules");
      return paths;
    };
    const resolve = function(request, _options) {
      const resolved = _resolveModule.applySyncPromise(void 0, [
        request,
        dirname
      ]);
      if (resolved === null) {
        const err = new Error("Cannot find module '" + request + "'");
        err.code = "MODULE_NOT_FOUND";
        throw err;
      }
      return resolved;
    };
    resolve.paths = resolvePaths;
    const rejectRestrictedBuiltin = function(request) {
      const normalized = String(request).replace(/^node:/, "");
      if (normalized === "http") {
        const error = new Error(`node:${normalized} is not available in the Agent OS guest runtime`);
        error.code = "ERR_ACCESS_DENIED";
        throw error;
      }
    };
    const requireFn = function(request) {
      rejectRestrictedBuiltin(request);
      return _requireFrom(request, dirname);
    };
    requireFn.resolve = resolve;
    requireFn.cache = _moduleCache;
    requireFn.main = void 0;
    requireFn.extensions = {
      ".js": function(_module, _filename) {
      },
      ".json": function(_module, _filename) {
      },
      ".node": function(_module, _filename) {
        throw new Error(".node extensions are not supported in sandbox");
      }
    };
    return requireFn;
  }
  var Module = class _Module {
    id;
    path;
    exports;
    filename;
    loaded;
    children;
    paths;
    parent;
    isPreloading;
    constructor(id, parent) {
      this.id = id;
      this.path = _pathDirname(id);
      this.exports = {};
      this.filename = id;
      this.loaded = false;
      this.children = [];
      this.paths = [];
      this.parent = parent;
      this.isPreloading = false;
      let current = this.path;
      while (current !== "/") {
        this.paths.push(current + "/node_modules");
        current = _pathDirname(current);
      }
      this.paths.push("/node_modules");
    }
    require(request) {
      return _requireFrom(request, this.path);
    }
    _compile(content, filename) {
      const wrapper = new Function(
        "exports",
        "require",
        "module",
        "__filename",
        "__dirname",
        content
      );
      const moduleRequire = (request) => {
        rejectRestrictedBuiltinRequest(request);
        return _requireFrom(request, this.path);
      };
      moduleRequire.resolve = (request) => {
        const resolved = _resolveModule.applySyncPromise(void 0, [
          request,
          this.path
        ]);
        if (resolved === null) {
          const err = new Error("Cannot find module '" + request + "'");
          err.code = "MODULE_NOT_FOUND";
          throw err;
        }
        return resolved;
      };
      wrapper(this.exports, moduleRequire, this, filename, this.path);
      this.loaded = true;
      return this.exports;
    }
    static _extensions = {
      ".js": function(module, filename) {
        const content = typeof _loadFile !== "undefined" ? _loadFile.applySyncPromise(void 0, [
          filename
        ]) : _requireFrom("fs", "/").readFileSync(filename, "utf8");
        module._compile(content, filename);
      },
      ".json": function(module, filename) {
        const content = typeof _loadFile !== "undefined" ? _loadFile.applySyncPromise(void 0, [
          filename
        ]) : _requireFrom("fs", "/").readFileSync(filename, "utf8");
        module.exports = JSON.parse(content);
      },
      ".node": function() {
        throw new Error(".node extensions are not supported in sandbox");
      }
    };
    static _cache = typeof _moduleCache !== "undefined" ? _moduleCache : {};
    static _resolveFilename(request, parent, _isMain, _options) {
      const parentDir = parent && parent.path ? parent.path : "/";
      const resolved = _resolveModule.applySyncPromise(void 0, [
        request,
        parentDir
      ]);
      if (resolved === null) {
        const err = new Error("Cannot find module '" + request + "'");
        err.code = "MODULE_NOT_FOUND";
        throw err;
      }
      return resolved;
    }
    static wrap(content) {
      return "(function (exports, require, module, __filename, __dirname) { " + content + "\n});";
    }
    static builtinModules = [
      "assert",
      "buffer",
      "child_process",
      "constants",
      "crypto",
      "dgram",
      "dns",
      "events",
      "fs",
      "http",
      "https",
      "net",
      "os",
      "path",
      "perf_hooks",
      "process",
      "punycode",
      "querystring",
      "sqlite",
      "stream",
      "string_decoder",
      "timers",
      "tls",
      "tty",
      "url",
      "util",
      "zlib",
      "vm",
      "module"
    ];
    static isBuiltin(moduleName) {
      const name = moduleName.replace(/^node:/, "");
      return _Module.builtinModules.includes(name);
    }
    static createRequire = createRequire;
    static syncBuiltinESMExports() {
    }
    static findSourceMap(_path) {
      return void 0;
    }
    static _nodeModulePaths(from) {
      const paths = [];
      let current = from;
      while (current !== "/") {
        paths.push(current + "/node_modules");
        current = _pathDirname(current);
        if (current === ".") break;
      }
      paths.push("/node_modules");
      return paths;
    }
    static _load(request, parent, _isMain) {
      const parentDir = parent && parent.path ? parent.path : "/";
      return _requireFrom(request, parentDir);
    }
    static runMain() {
    }
  };
  var SourceMap = class {
    constructor(_payload) {
      throw new Error("SourceMap is not implemented in sandbox");
    }
    get payload() {
      throw new Error("SourceMap is not implemented in sandbox");
    }
    set payload(_value) {
      throw new Error("SourceMap is not implemented in sandbox");
    }
    findEntry(_line, _column) {
      throw new Error("SourceMap is not implemented in sandbox");
    }
  };
  var moduleModule = {
    Module,
    createRequire,
    // Module._extensions (deprecated alias)
    _extensions: Module._extensions,
    // Module._cache reference
    _cache: Module._cache,
    // Built-in module list
    builtinModules: Module.builtinModules,
    // isBuiltin check
    isBuiltin: Module.isBuiltin,
    // Module._resolveFilename (internal but sometimes used)
    _resolveFilename: Module._resolveFilename,
    // wrap function
    wrap: Module.wrap,
    // syncBuiltinESMExports (stub for ESM interop)
    syncBuiltinESMExports: Module.syncBuiltinESMExports,
    // findSourceMap (stub)
    findSourceMap: Module.findSourceMap,
    // SourceMap class (stub)
    SourceMap
  };
  exposeCustomGlobal("_moduleModule", moduleModule);
  var builtinTimersModule = {
    clearImmediate: globalThis.clearImmediate ?? function() {
    },
    clearInterval: globalThis.clearInterval ?? function() {
    },
    clearTimeout: globalThis.clearTimeout ?? function() {
    },
    setImmediate: globalThis.setImmediate ?? function(callback, ...args) {
      return globalThis.setTimeout?.(() => callback(...args), 0);
    },
    setInterval: globalThis.setInterval ?? function(callback, delay, ...args) {
      return globalThis.setTimeout?.(() => callback(...args), delay ?? 0);
    },
    setTimeout: globalThis.setTimeout ?? function() {
      return void 0;
    }
  };
  function unwrapStdlibModule(moduleNamespace) {
    if (moduleNamespace && typeof moduleNamespace === "object" && moduleNamespace.default != null) {
      return moduleNamespace.default;
    }
    return moduleNamespace;
  }
  function cloneStdlibModule(moduleNamespace) {
    const resolved = unwrapStdlibModule(moduleNamespace);
    if (resolved == null) {
      return resolved;
    }
    if (typeof resolved === "function") {
      return resolved;
    }
    if (typeof resolved === "object") {
      return { ...resolved };
    }
    return resolved;
  }
  function defineMissingModuleProperty(target, key, value) {
    if (target != null && typeof target[key] === "undefined") {
      target[key] = value;
    }
  }
  function trimNonRootTrailingSlash(pathValue) {
    return typeof pathValue === "string" && pathValue.length > 1 && pathValue.endsWith("/") ? pathValue.slice(0, -1) : pathValue;
  }
  var builtinBufferStdlibModule = cloneStdlibModule(bufferStdlibModuleNs);
  var builtinConstantsStdlibModule = cloneStdlibModule(constantsStdlibModuleNs);
  var builtinEventsStdlibModule = cloneStdlibModule(eventsStdlibModuleNs);
  var builtinPathStdlibModule = cloneStdlibModule(pathStdlibModuleNs);
  if (builtinPathStdlibModule?.normalize) {
    const builtinPathNormalize = builtinPathStdlibModule.normalize.bind(builtinPathStdlibModule);
    builtinPathStdlibModule.normalize = function(pathValue) {
      return trimNonRootTrailingSlash(builtinPathNormalize(pathValue));
    };
  }
  var builtinPunycodeStdlibModule = cloneStdlibModule(punycodeStdlibModuleNs);
  var builtinQuerystringStdlibModule = cloneStdlibModule(querystringStdlibModuleNs);
  var builtinStreamStdlibModule = cloneStdlibModule(streamStdlibModuleNs);
  defineMissingModuleProperty(builtinStreamStdlibModule, "isReadable", (stream) => {
    return Boolean(stream) && stream.readable !== false && stream.destroyed !== true;
  });
  defineMissingModuleProperty(builtinStreamStdlibModule, "isErrored", (stream) => {
    return stream?.errored != null;
  });
  defineMissingModuleProperty(builtinStreamStdlibModule, "isDisturbed", (stream) => {
    return Boolean(stream?.locked || stream?.disturbed === true || stream?.readableDidRead === true);
  });
  var builtinStringDecoderStdlibModule = cloneStdlibModule(stringDecoderStdlibModuleNs);
  var builtinUrlStdlibModule = cloneStdlibModule(urlStdlibModuleNs);
  function normalizeBuiltinRequest(request) {
    return String(request).replace(/^node:/, "");
  }
  function rejectRestrictedBuiltinRequest(request) {
    const normalized = normalizeBuiltinRequest(request);
    return normalized;
  }
  function loadBuiltinModule(request) {
    const normalized = rejectRestrictedBuiltinRequest(request);
    switch (normalized) {
      case "assert":
        return globalThis.__agentOsBuiltinAssertModule;
      case "buffer":
        return builtinBufferStdlibModule;
      case "crypto":
        return builtinCryptoModule;
      case "http":
        return _httpModule;
      case "events":
        return builtinEventsStdlibModule;
      case "fs":
        return _fsModule;
      case "os":
        return _osModule;
      case "path":
        return builtinPathStdlibModule;
      case "perf_hooks":
        return builtinPerfHooksModule;
      case "process":
        return process_default;
      case "punycode":
        return builtinPunycodeStdlibModule;
      case "querystring":
        return builtinQuerystringStdlibModule;
      case "stream":
        return builtinStreamStdlibModule;
      case "string_decoder":
        return builtinStringDecoderStdlibModule;
      case "timers":
        return builtinTimersModule;
      case "url":
        return builtinUrlStdlibModule;
      case "util":
        return globalThis.__agentOsBuiltinUtilModule;
      case "child_process":
        return _childProcessModule;
      case "constants":
        return builtinConstantsStdlibModule;
      case "dns":
        return _dnsModule;
      case "net":
        return _netModule;
      case "tls":
        return _tlsModule;
      case "tty":
        return builtinTtyModule;
      case "dgram":
        return _dgramModule;
      case "sqlite":
        return _sqliteModule;
      case "https":
        return _httpsModule;
      case "module":
        return _moduleModule;
      case "vm":
        return builtinVmModule;
      case "zlib":
        return globalThis.__agentOsBuiltinZlibModule;
      default: {
        const error = new Error(`Cannot find module '${request}'`);
        error.code = "MODULE_NOT_FOUND";
        throw error;
      }
    }
  }
  function requireFrom(request, parentDir) {
    const parentPath = typeof parentDir === "string" ? parentDir : "/";
    if (Module.isBuiltin(request)) {
      try {
        return loadBuiltinModule(request);
      } catch (error) {
        if (error?.code !== "MODULE_NOT_FOUND") {
          throw error;
        }
      }
    }
    const resolved = _resolveModule.applySyncPromise(void 0, [
      request,
      parentPath
    ]);
    if (resolved === null) {
      const error = new Error(`Cannot find module '${request}'`);
      error.code = "MODULE_NOT_FOUND";
      throw error;
    }
    if (Object.prototype.hasOwnProperty.call(_moduleCache, resolved)) {
      return _moduleCache[resolved].exports;
    }
    const module = new Module(resolved, { path: parentPath });
    _moduleCache[resolved] = module;
    try {
      const extension = resolved.endsWith(".json") ? ".json" : ".js";
      const loader = Module._extensions[extension] ?? Module._extensions[".js"];
      loader(module, resolved);
      module.loaded = true;
      return module.exports;
    } catch (error) {
      delete _moduleCache[resolved];
      throw error;
    }
  }
  exposeCustomGlobal("_requireFrom", requireFrom);
  var module_default = moduleModule;

  // .agent/recovery/secure-exec/nodejs/src/bridge/index.ts
  var index_default = fs_default;
  setupGlobals();
  return __toCommonJS(index_exports);
})();
/*! Bundled license information:

ieee754/index.js:
  (*! ieee754. BSD-3-Clause License. Feross Aboukhadijeh <https://feross.org/opensource> *)

buffer/index.js:
  (*!
   * The buffer module from node.js, for the browser.
   *
   * @author   Feross Aboukhadijeh <https://feross.org>
   * @license  MIT
   *)
*/
