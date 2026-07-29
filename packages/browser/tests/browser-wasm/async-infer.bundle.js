var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __esm = (fn, res) => function __init() {
  return fn && (res = (0, fn[__getOwnPropNames(fn)[0]])(fn = 0)), res;
};
var __commonJS = (cb, mod) => function __require() {
  return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
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

// ../../node_modules/.pnpm/base64-js@1.5.1/node_modules/base64-js/index.js
var require_base64_js = __commonJS({
  "../../node_modules/.pnpm/base64-js@1.5.1/node_modules/base64-js/index.js"(exports) {
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

// ../../node_modules/.pnpm/ieee754@1.2.1/node_modules/ieee754/index.js
var require_ieee754 = __commonJS({
  "../../node_modules/.pnpm/ieee754@1.2.1/node_modules/ieee754/index.js"(exports) {
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

// ../../node_modules/.pnpm/buffer@6.0.3/node_modules/buffer/index.js
var require_buffer = __commonJS({
  "../../node_modules/.pnpm/buffer@6.0.3/node_modules/buffer/index.js"(exports) {
    "use strict";
    var base64 = require_base64_js();
    var ieee754 = require_ieee754();
    var customInspectSymbol = typeof Symbol === "function" && typeof Symbol["for"] === "function" ? Symbol["for"]("nodejs.util.inspect.custom") : null;
    exports.Buffer = Buffer2;
    exports.SlowBuffer = SlowBuffer;
    exports.INSPECT_MAX_BYTES = 50;
    var K_MAX_LENGTH = 2147483647;
    exports.kMaxLength = K_MAX_LENGTH;
    Buffer2.TYPED_ARRAY_SUPPORT = typedArraySupport();
    if (!Buffer2.TYPED_ARRAY_SUPPORT && typeof console !== "undefined" && typeof console.error === "function") {
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
    Object.defineProperty(Buffer2.prototype, "parent", {
      enumerable: true,
      get: function() {
        if (!Buffer2.isBuffer(this)) return void 0;
        return this.buffer;
      }
    });
    Object.defineProperty(Buffer2.prototype, "offset", {
      enumerable: true,
      get: function() {
        if (!Buffer2.isBuffer(this)) return void 0;
        return this.byteOffset;
      }
    });
    function createBuffer(length) {
      if (length > K_MAX_LENGTH) {
        throw new RangeError('The value "' + length + '" is invalid for option "size"');
      }
      const buf = new Uint8Array(length);
      Object.setPrototypeOf(buf, Buffer2.prototype);
      return buf;
    }
    function Buffer2(arg, encodingOrOffset, length) {
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
    Buffer2.poolSize = 8192;
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
        return Buffer2.from(valueOf, encodingOrOffset, length);
      }
      const b = fromObject(value);
      if (b) return b;
      if (typeof Symbol !== "undefined" && Symbol.toPrimitive != null && typeof value[Symbol.toPrimitive] === "function") {
        return Buffer2.from(value[Symbol.toPrimitive]("string"), encodingOrOffset, length);
      }
      throw new TypeError(
        "The first argument must be one of type string, Buffer, ArrayBuffer, Array, or Array-like Object. Received type " + typeof value
      );
    }
    Buffer2.from = function(value, encodingOrOffset, length) {
      return from(value, encodingOrOffset, length);
    };
    Object.setPrototypeOf(Buffer2.prototype, Uint8Array.prototype);
    Object.setPrototypeOf(Buffer2, Uint8Array);
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
    Buffer2.alloc = function(size, fill, encoding) {
      return alloc(size, fill, encoding);
    };
    function allocUnsafe(size) {
      assertSize(size);
      return createBuffer(size < 0 ? 0 : checked(size) | 0);
    }
    Buffer2.allocUnsafe = function(size) {
      return allocUnsafe(size);
    };
    Buffer2.allocUnsafeSlow = function(size) {
      return allocUnsafe(size);
    };
    function fromString(string, encoding) {
      if (typeof encoding !== "string" || encoding === "") {
        encoding = "utf8";
      }
      if (!Buffer2.isEncoding(encoding)) {
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
      Object.setPrototypeOf(buf, Buffer2.prototype);
      return buf;
    }
    function fromObject(obj) {
      if (Buffer2.isBuffer(obj)) {
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
      return Buffer2.alloc(+length);
    }
    Buffer2.isBuffer = function isBuffer(b) {
      return b != null && b._isBuffer === true && b !== Buffer2.prototype;
    };
    Buffer2.compare = function compare(a, b) {
      if (isInstance(a, Uint8Array)) a = Buffer2.from(a, a.offset, a.byteLength);
      if (isInstance(b, Uint8Array)) b = Buffer2.from(b, b.offset, b.byteLength);
      if (!Buffer2.isBuffer(a) || !Buffer2.isBuffer(b)) {
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
    Buffer2.isEncoding = function isEncoding(encoding) {
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
    Buffer2.concat = function concat(list, length) {
      if (!Array.isArray(list)) {
        throw new TypeError('"list" argument must be an Array of Buffers');
      }
      if (list.length === 0) {
        return Buffer2.alloc(0);
      }
      let i;
      if (length === void 0) {
        length = 0;
        for (i = 0; i < list.length; ++i) {
          length += list[i].length;
        }
      }
      const buffer = Buffer2.allocUnsafe(length);
      let pos = 0;
      for (i = 0; i < list.length; ++i) {
        let buf = list[i];
        if (isInstance(buf, Uint8Array)) {
          if (pos + buf.length > buffer.length) {
            if (!Buffer2.isBuffer(buf)) buf = Buffer2.from(buf);
            buf.copy(buffer, pos);
          } else {
            Uint8Array.prototype.set.call(
              buffer,
              buf,
              pos
            );
          }
        } else if (!Buffer2.isBuffer(buf)) {
          throw new TypeError('"list" argument must be an Array of Buffers');
        } else {
          buf.copy(buffer, pos);
        }
        pos += buf.length;
      }
      return buffer;
    };
    function byteLength(string, encoding) {
      if (Buffer2.isBuffer(string)) {
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
            return base64ToBytes2(string).length;
          default:
            if (loweredCase) {
              return mustMatch ? -1 : utf8ToBytes(string).length;
            }
            encoding = ("" + encoding).toLowerCase();
            loweredCase = true;
        }
      }
    }
    Buffer2.byteLength = byteLength;
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
    Buffer2.prototype._isBuffer = true;
    function swap(b, n, m) {
      const i = b[n];
      b[n] = b[m];
      b[m] = i;
    }
    Buffer2.prototype.swap16 = function swap16() {
      const len = this.length;
      if (len % 2 !== 0) {
        throw new RangeError("Buffer size must be a multiple of 16-bits");
      }
      for (let i = 0; i < len; i += 2) {
        swap(this, i, i + 1);
      }
      return this;
    };
    Buffer2.prototype.swap32 = function swap32() {
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
    Buffer2.prototype.swap64 = function swap64() {
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
    Buffer2.prototype.toString = function toString() {
      const length = this.length;
      if (length === 0) return "";
      if (arguments.length === 0) return utf8Slice(this, 0, length);
      return slowToString.apply(this, arguments);
    };
    Buffer2.prototype.toLocaleString = Buffer2.prototype.toString;
    Buffer2.prototype.equals = function equals(b) {
      if (!Buffer2.isBuffer(b)) throw new TypeError("Argument must be a Buffer");
      if (this === b) return true;
      return Buffer2.compare(this, b) === 0;
    };
    Buffer2.prototype.inspect = function inspect() {
      let str = "";
      const max = exports.INSPECT_MAX_BYTES;
      str = this.toString("hex", 0, max).replace(/(.{2})/g, "$1 ").trim();
      if (this.length > max) str += " ... ";
      return "<Buffer " + str + ">";
    };
    if (customInspectSymbol) {
      Buffer2.prototype[customInspectSymbol] = Buffer2.prototype.inspect;
    }
    Buffer2.prototype.compare = function compare(target, start, end, thisStart, thisEnd) {
      if (isInstance(target, Uint8Array)) {
        target = Buffer2.from(target, target.offset, target.byteLength);
      }
      if (!Buffer2.isBuffer(target)) {
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
        val = Buffer2.from(val, encoding);
      }
      if (Buffer2.isBuffer(val)) {
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
    Buffer2.prototype.includes = function includes(val, byteOffset, encoding) {
      return this.indexOf(val, byteOffset, encoding) !== -1;
    };
    Buffer2.prototype.indexOf = function indexOf(val, byteOffset, encoding) {
      return bidirectionalIndexOf(this, val, byteOffset, encoding, true);
    };
    Buffer2.prototype.lastIndexOf = function lastIndexOf(val, byteOffset, encoding) {
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
      return blitBuffer(base64ToBytes2(string), buf, offset, length);
    }
    function ucs2Write(buf, string, offset, length) {
      return blitBuffer(utf16leToBytes(string, buf.length - offset), buf, offset, length);
    }
    Buffer2.prototype.write = function write(string, offset, length, encoding) {
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
    Buffer2.prototype.toJSON = function toJSON() {
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
    Buffer2.prototype.slice = function slice(start, end) {
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
      Object.setPrototypeOf(newBuf, Buffer2.prototype);
      return newBuf;
    };
    function checkOffset(offset, ext, length) {
      if (offset % 1 !== 0 || offset < 0) throw new RangeError("offset is not uint");
      if (offset + ext > length) throw new RangeError("Trying to access beyond buffer length");
    }
    Buffer2.prototype.readUintLE = Buffer2.prototype.readUIntLE = function readUIntLE(offset, byteLength2, noAssert) {
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
    Buffer2.prototype.readUintBE = Buffer2.prototype.readUIntBE = function readUIntBE(offset, byteLength2, noAssert) {
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
    Buffer2.prototype.readUint8 = Buffer2.prototype.readUInt8 = function readUInt8(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 1, this.length);
      return this[offset];
    };
    Buffer2.prototype.readUint16LE = Buffer2.prototype.readUInt16LE = function readUInt16LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      return this[offset] | this[offset + 1] << 8;
    };
    Buffer2.prototype.readUint16BE = Buffer2.prototype.readUInt16BE = function readUInt16BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      return this[offset] << 8 | this[offset + 1];
    };
    Buffer2.prototype.readUint32LE = Buffer2.prototype.readUInt32LE = function readUInt32LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return (this[offset] | this[offset + 1] << 8 | this[offset + 2] << 16) + this[offset + 3] * 16777216;
    };
    Buffer2.prototype.readUint32BE = Buffer2.prototype.readUInt32BE = function readUInt32BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return this[offset] * 16777216 + (this[offset + 1] << 16 | this[offset + 2] << 8 | this[offset + 3]);
    };
    Buffer2.prototype.readBigUInt64LE = defineBigIntMethod(function readBigUInt64LE(offset) {
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
    Buffer2.prototype.readBigUInt64BE = defineBigIntMethod(function readBigUInt64BE(offset) {
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
    Buffer2.prototype.readIntLE = function readIntLE(offset, byteLength2, noAssert) {
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
    Buffer2.prototype.readIntBE = function readIntBE(offset, byteLength2, noAssert) {
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
    Buffer2.prototype.readInt8 = function readInt8(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 1, this.length);
      if (!(this[offset] & 128)) return this[offset];
      return (255 - this[offset] + 1) * -1;
    };
    Buffer2.prototype.readInt16LE = function readInt16LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      const val = this[offset] | this[offset + 1] << 8;
      return val & 32768 ? val | 4294901760 : val;
    };
    Buffer2.prototype.readInt16BE = function readInt16BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      const val = this[offset + 1] | this[offset] << 8;
      return val & 32768 ? val | 4294901760 : val;
    };
    Buffer2.prototype.readInt32LE = function readInt32LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return this[offset] | this[offset + 1] << 8 | this[offset + 2] << 16 | this[offset + 3] << 24;
    };
    Buffer2.prototype.readInt32BE = function readInt32BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return this[offset] << 24 | this[offset + 1] << 16 | this[offset + 2] << 8 | this[offset + 3];
    };
    Buffer2.prototype.readBigInt64LE = defineBigIntMethod(function readBigInt64LE(offset) {
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
    Buffer2.prototype.readBigInt64BE = defineBigIntMethod(function readBigInt64BE(offset) {
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
    Buffer2.prototype.readFloatLE = function readFloatLE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return ieee754.read(this, offset, true, 23, 4);
    };
    Buffer2.prototype.readFloatBE = function readFloatBE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return ieee754.read(this, offset, false, 23, 4);
    };
    Buffer2.prototype.readDoubleLE = function readDoubleLE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 8, this.length);
      return ieee754.read(this, offset, true, 52, 8);
    };
    Buffer2.prototype.readDoubleBE = function readDoubleBE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 8, this.length);
      return ieee754.read(this, offset, false, 52, 8);
    };
    function checkInt(buf, value, offset, ext, max, min) {
      if (!Buffer2.isBuffer(buf)) throw new TypeError('"buffer" argument must be a Buffer instance');
      if (value > max || value < min) throw new RangeError('"value" argument is out of bounds');
      if (offset + ext > buf.length) throw new RangeError("Index out of range");
    }
    Buffer2.prototype.writeUintLE = Buffer2.prototype.writeUIntLE = function writeUIntLE(value, offset, byteLength2, noAssert) {
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
    Buffer2.prototype.writeUintBE = Buffer2.prototype.writeUIntBE = function writeUIntBE(value, offset, byteLength2, noAssert) {
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
    Buffer2.prototype.writeUint8 = Buffer2.prototype.writeUInt8 = function writeUInt8(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 1, 255, 0);
      this[offset] = value & 255;
      return offset + 1;
    };
    Buffer2.prototype.writeUint16LE = Buffer2.prototype.writeUInt16LE = function writeUInt16LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 65535, 0);
      this[offset] = value & 255;
      this[offset + 1] = value >>> 8;
      return offset + 2;
    };
    Buffer2.prototype.writeUint16BE = Buffer2.prototype.writeUInt16BE = function writeUInt16BE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 65535, 0);
      this[offset] = value >>> 8;
      this[offset + 1] = value & 255;
      return offset + 2;
    };
    Buffer2.prototype.writeUint32LE = Buffer2.prototype.writeUInt32LE = function writeUInt32LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 4, 4294967295, 0);
      this[offset + 3] = value >>> 24;
      this[offset + 2] = value >>> 16;
      this[offset + 1] = value >>> 8;
      this[offset] = value & 255;
      return offset + 4;
    };
    Buffer2.prototype.writeUint32BE = Buffer2.prototype.writeUInt32BE = function writeUInt32BE(value, offset, noAssert) {
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
    Buffer2.prototype.writeBigUInt64LE = defineBigIntMethod(function writeBigUInt64LE(value, offset = 0) {
      return wrtBigUInt64LE(this, value, offset, BigInt(0), BigInt("0xffffffffffffffff"));
    });
    Buffer2.prototype.writeBigUInt64BE = defineBigIntMethod(function writeBigUInt64BE(value, offset = 0) {
      return wrtBigUInt64BE(this, value, offset, BigInt(0), BigInt("0xffffffffffffffff"));
    });
    Buffer2.prototype.writeIntLE = function writeIntLE(value, offset, byteLength2, noAssert) {
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
    Buffer2.prototype.writeIntBE = function writeIntBE(value, offset, byteLength2, noAssert) {
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
    Buffer2.prototype.writeInt8 = function writeInt8(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 1, 127, -128);
      if (value < 0) value = 255 + value + 1;
      this[offset] = value & 255;
      return offset + 1;
    };
    Buffer2.prototype.writeInt16LE = function writeInt16LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 32767, -32768);
      this[offset] = value & 255;
      this[offset + 1] = value >>> 8;
      return offset + 2;
    };
    Buffer2.prototype.writeInt16BE = function writeInt16BE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 32767, -32768);
      this[offset] = value >>> 8;
      this[offset + 1] = value & 255;
      return offset + 2;
    };
    Buffer2.prototype.writeInt32LE = function writeInt32LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 4, 2147483647, -2147483648);
      this[offset] = value & 255;
      this[offset + 1] = value >>> 8;
      this[offset + 2] = value >>> 16;
      this[offset + 3] = value >>> 24;
      return offset + 4;
    };
    Buffer2.prototype.writeInt32BE = function writeInt32BE(value, offset, noAssert) {
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
    Buffer2.prototype.writeBigInt64LE = defineBigIntMethod(function writeBigInt64LE(value, offset = 0) {
      return wrtBigUInt64LE(this, value, offset, -BigInt("0x8000000000000000"), BigInt("0x7fffffffffffffff"));
    });
    Buffer2.prototype.writeBigInt64BE = defineBigIntMethod(function writeBigInt64BE(value, offset = 0) {
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
    Buffer2.prototype.writeFloatLE = function writeFloatLE(value, offset, noAssert) {
      return writeFloat(this, value, offset, true, noAssert);
    };
    Buffer2.prototype.writeFloatBE = function writeFloatBE(value, offset, noAssert) {
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
    Buffer2.prototype.writeDoubleLE = function writeDoubleLE(value, offset, noAssert) {
      return writeDouble(this, value, offset, true, noAssert);
    };
    Buffer2.prototype.writeDoubleBE = function writeDoubleBE(value, offset, noAssert) {
      return writeDouble(this, value, offset, false, noAssert);
    };
    Buffer2.prototype.copy = function copy(target, targetStart, start, end) {
      if (!Buffer2.isBuffer(target)) throw new TypeError("argument should be a Buffer");
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
    Buffer2.prototype.fill = function fill(val, start, end, encoding) {
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
        if (typeof encoding === "string" && !Buffer2.isEncoding(encoding)) {
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
        const bytes = Buffer2.isBuffer(val) ? val : Buffer2.from(val, encoding);
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
    function base64ToBytes2(str) {
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

// ../../../agent-os/packages/core/dist/bytes.js
function toExactArrayBuffer(value) {
  return value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength);
}
function toExactUint8Array(value) {
  return Uint8Array.from(value);
}
var init_bytes = __esm({
  "../../../agent-os/packages/core/dist/bytes.js"() {
    "use strict";
  }
});

// ../../../agent-os/packages/core/dist/frame-payload-codec.js
var init_frame_payload_codec = __esm({
  "../../../agent-os/packages/core/dist/frame-payload-codec.js"() {
    "use strict";
  }
});

// ../../../agent-os/packages/core/dist/ext.js
function toGeneratedExtEnvelope(envelope) {
  return {
    namespace: envelope.namespace,
    payload: toExactArrayBuffer(envelope.payload)
  };
}
function fromGeneratedExtEnvelope(envelope) {
  return {
    namespace: envelope.namespace,
    payload: Buffer.from(envelope.payload)
  };
}
var init_ext = __esm({
  "../../../agent-os/packages/core/dist/ext.js"() {
    "use strict";
    init_bytes();
  }
});

// ../../../agent-os/packages/core/dist/json.js
function stringifyJsonUtf8(value, context) {
  try {
    const encoded = JSON.stringify(value);
    if (encoded === void 0) {
      throw new Error(`${context} must be JSON-serializable`);
    }
    return encoded;
  } catch (error) {
    throw new Error(`${context} must be JSON-serializable: ${error instanceof Error ? error.message : String(error)}`);
  }
}
function parseJsonUtf8(value, context) {
  try {
    return JSON.parse(value);
  } catch (error) {
    throw new Error(`invalid ${context} JSON payload: ${error instanceof Error ? error.message : String(error)}`);
  }
}
var init_json = __esm({
  "../../../agent-os/packages/core/dist/json.js"() {
    "use strict";
  }
});

// ../../../agent-os/packages/core/dist/numbers.js
function bigIntToSafeNumber(value, context) {
  const max = BigInt(Number.MAX_SAFE_INTEGER);
  const min = BigInt(Number.MIN_SAFE_INTEGER);
  if (value > max || value < min) {
    throw new Error(`${context} exceeds JavaScript safe integer range`);
  }
  return Number(value);
}
var init_numbers = __esm({
  "../../../agent-os/packages/core/dist/numbers.js"() {
    "use strict";
  }
});

// ../../../agent-os/packages/core/dist/callbacks.js
function fromGeneratedSidecarRequestPayload(payload) {
  switch (payload.tag) {
    case "HostCallbackRequest":
      return {
        type: "host_callback",
        invocation_id: payload.val.invocationId,
        callback_key: payload.val.callbackKey,
        input: parseJsonUtf8(payload.val.input, "host callback input"),
        timeout_ms: bigIntToSafeNumber(payload.val.timeoutMs, "host callback timeout")
      };
    case "JsBridgeCallRequest":
      return {
        type: "js_bridge_call",
        call_id: payload.val.callId,
        mount_id: payload.val.mountId,
        operation: payload.val.operation,
        args: parseJsonUtf8(payload.val.args, "js bridge call args")
      };
    case "ExtEnvelope":
      return {
        type: "ext",
        envelope: fromGeneratedExtEnvelope(payload.val)
      };
  }
}
function toGeneratedSidecarResponsePayload(payload) {
  switch (payload.type) {
    case "host_callback_result":
      return {
        tag: "HostCallbackResultResponse",
        val: {
          invocationId: payload.invocation_id,
          result: payload.result === void 0 ? null : stringifyJsonUtf8(payload.result, "host_callback_result.result"),
          error: payload.error ?? null
        }
      };
    case "js_bridge_result":
      return {
        tag: "JsBridgeResultResponse",
        val: {
          callId: payload.call_id,
          result: payload.result === void 0 ? null : stringifyJsonUtf8(payload.result, "js_bridge_result.result"),
          error: payload.error ?? null
        }
      };
    case "ext_result":
      return {
        tag: "ExtEnvelope",
        val: toGeneratedExtEnvelope(payload.envelope)
      };
  }
}
var init_callbacks = __esm({
  "../../../agent-os/packages/core/dist/callbacks.js"() {
    "use strict";
    init_ext();
    init_json();
    init_numbers();
  }
});

// ../../../agent-os/packages/core/dist/ownership.js
function toGeneratedOwnershipScope(ownership) {
  switch (ownership.scope) {
    case "connection":
      return {
        tag: "ConnectionOwnership",
        val: { connectionId: ownership.connection_id }
      };
    case "session":
      return {
        tag: "SessionOwnership",
        val: {
          connectionId: ownership.connection_id,
          sessionId: ownership.session_id
        }
      };
    case "vm":
      return {
        tag: "VmOwnership",
        val: {
          connectionId: ownership.connection_id,
          sessionId: ownership.session_id,
          vmId: ownership.vm_id
        }
      };
  }
}
function fromGeneratedOwnershipScope(ownership) {
  switch (ownership.tag) {
    case "ConnectionOwnership":
      return {
        scope: "connection",
        connection_id: ownership.val.connectionId
      };
    case "SessionOwnership":
      return {
        scope: "session",
        connection_id: ownership.val.connectionId,
        session_id: ownership.val.sessionId
      };
    case "VmOwnership":
      return {
        scope: "vm",
        connection_id: ownership.val.connectionId,
        session_id: ownership.val.sessionId,
        vm_id: ownership.val.vmId
      };
  }
}
var init_ownership = __esm({
  "../../../agent-os/packages/core/dist/ownership.js"() {
    "use strict";
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/imports/dev.js
var DEV;
var init_dev = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/imports/dev.js"() {
    DEV = false;
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/assert.js
function assert(test, message = "") {
  if (!test) {
    const e = new AssertionError(message);
    V8Error.captureStackTrace?.(e, assert);
    throw e;
  }
}
var V8Error, AssertionError;
var init_assert = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/assert.js"() {
    init_dev();
    V8Error = Error;
    AssertionError = class extends Error {
      constructor() {
        super(...arguments);
        this.name = "AssertionError";
      }
    };
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/validator.js
function isI32(val) {
  return val === (val | 0);
}
function isI64(val) {
  return val === BigInt.asIntN(64, val);
}
function isU8(val) {
  return val === (val & 255);
}
function isU16(val) {
  return val === (val & 65535);
}
function isU32(val) {
  return val === val >>> 0;
}
function isU64(val) {
  return val === BigInt.asUintN(64, val);
}
function isU64Safe(val) {
  return Number.isSafeInteger(val) && val >= 0;
}
var init_validator = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/validator.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/constants.js
var TEXT_DECODER_THRESHOLD, TEXT_ENCODER_THRESHOLD, INT_SAFE_MAX_BYTE_COUNT, UINT_SAFE32_MAX_BYTE_COUNT, INVALID_UTF8_STRING, NON_CANONICAL_REPRESENTATION, TOO_LARGE_BUFFER, TOO_LARGE_NUMBER, IS_LITTLE_ENDIAN_PLATFORM;
var init_constants = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/constants.js"() {
    TEXT_DECODER_THRESHOLD = 256;
    TEXT_ENCODER_THRESHOLD = 256;
    INT_SAFE_MAX_BYTE_COUNT = 8;
    UINT_SAFE32_MAX_BYTE_COUNT = 5;
    INVALID_UTF8_STRING = "invalid UTF-8 string";
    NON_CANONICAL_REPRESENTATION = "must be canonical";
    TOO_LARGE_BUFFER = "too large buffer";
    TOO_LARGE_NUMBER = "too large number";
    IS_LITTLE_ENDIAN_PLATFORM = /* @__PURE__ */ new DataView(Uint16Array.of(1).buffer).getUint8(0) === 1;
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/bare-error.js
var BareError;
var init_bare_error = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/bare-error.js"() {
    BareError = class extends Error {
      constructor(offset, issue, opts) {
        super(`(byte:${offset}) ${issue}`);
        this.name = "BareError";
        this.issue = issue;
        this.offset = offset;
        this.cause = opts?.cause;
      }
    };
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/byte-cursor.js
function check(bc, min) {
  if (DEV) {
    assert(isU32(min));
  }
  if (bc.offset + min > bc.bytes.length) {
    throw new BareError(bc.offset, "missing bytes");
  }
}
function reserve(bc, min) {
  if (DEV) {
    assert(isU32(min));
  }
  const minLen = bc.offset + min | 0;
  if (minLen > bc.bytes.length) {
    grow(bc, minLen);
  }
}
function grow(bc, minLen) {
  if (minLen > bc.config.maxBufferLength) {
    throw new BareError(0, TOO_LARGE_BUFFER);
  }
  const buffer = bc.bytes.buffer;
  let newBytes;
  if (isEs2024ArrayBufferLike(buffer) && // Make sure that the view covers the end of the buffer.
  // If it is not the case, this indicates that the user don't want
  // to override the trailing bytes.
  bc.bytes.byteOffset + bc.bytes.byteLength === buffer.byteLength && bc.bytes.byteLength + minLen <= buffer.maxByteLength) {
    const newLen = Math.min(minLen << 1, bc.config.maxBufferLength, buffer.maxByteLength);
    if (buffer instanceof ArrayBuffer) {
      buffer.resize(newLen);
    } else {
      buffer.grow(newLen);
    }
    newBytes = new Uint8Array(buffer, bc.bytes.byteOffset, newLen);
  } else {
    const newLen = Math.min(minLen << 1, bc.config.maxBufferLength);
    newBytes = new Uint8Array(newLen);
    newBytes.set(bc.bytes);
  }
  bc.bytes = newBytes;
  bc.view = new DataView(newBytes.buffer);
}
function isEs2024ArrayBufferLike(buffer) {
  return "maxByteLength" in buffer;
}
var ByteCursor;
var init_byte_cursor = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/byte-cursor.js"() {
    init_assert();
    init_constants();
    init_validator();
    init_bare_error();
    ByteCursor = class {
      /**
       * @throws {BareError} Buffer exceeds `config.maxBufferLength`
       */
      constructor(bytes, config) {
        this.offset = 0;
        if (bytes.length > config.maxBufferLength) {
          throw new BareError(0, TOO_LARGE_BUFFER);
        }
        this.bytes = bytes;
        this.config = config;
        this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
      }
    };
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/fixed-primitive.js
function readBool(bc) {
  const val = readU8(bc);
  if (val > 1) {
    bc.offset--;
    throw new BareError(bc.offset, "a bool must be equal to 0 or 1");
  }
  return val > 0;
}
function writeBool(bc, x) {
  writeU8(bc, x ? 1 : 0);
}
function readI32(bc) {
  check(bc, 4);
  const result = bc.view.getInt32(bc.offset, true);
  bc.offset += 4;
  return result;
}
function writeI32(bc, x) {
  if (DEV) {
    assert(isI32(x), TOO_LARGE_NUMBER);
  }
  reserve(bc, 4);
  bc.view.setInt32(bc.offset, x, true);
  bc.offset += 4;
}
function readI64(bc) {
  check(bc, 8);
  const result = bc.view.getBigInt64(bc.offset, true);
  bc.offset += 8;
  return result;
}
function writeI64(bc, x) {
  if (DEV) {
    assert(isI64(x), TOO_LARGE_NUMBER);
  }
  reserve(bc, 8);
  bc.view.setBigInt64(bc.offset, x, true);
  bc.offset += 8;
}
function readU8(bc) {
  check(bc, 1);
  return bc.bytes[bc.offset++];
}
function writeU8(bc, x) {
  if (DEV) {
    assert(isU8(x), TOO_LARGE_NUMBER);
  }
  reserve(bc, 1);
  bc.bytes[bc.offset++] = x;
}
function readU16(bc) {
  check(bc, 2);
  const result = bc.view.getUint16(bc.offset, true);
  bc.offset += 2;
  return result;
}
function writeU16(bc, x) {
  if (DEV) {
    assert(isU16(x), TOO_LARGE_NUMBER);
  }
  reserve(bc, 2);
  bc.view.setUint16(bc.offset, x, true);
  bc.offset += 2;
}
function readU32(bc) {
  check(bc, 4);
  const result = bc.view.getUint32(bc.offset, true);
  bc.offset += 4;
  return result;
}
function writeU32(bc, x) {
  if (DEV) {
    assert(isU32(x), TOO_LARGE_NUMBER);
  }
  reserve(bc, 4);
  bc.view.setUint32(bc.offset, x, true);
  bc.offset += 4;
}
function readU64(bc) {
  check(bc, 8);
  const result = bc.view.getBigUint64(bc.offset, true);
  bc.offset += 8;
  return result;
}
function writeU64(bc, x) {
  if (DEV) {
    assert(isU64(x), TOO_LARGE_NUMBER);
  }
  reserve(bc, 8);
  bc.view.setBigUint64(bc.offset, x, true);
  bc.offset += 8;
}
var init_fixed_primitive = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/fixed-primitive.js"() {
    init_bare_error();
    init_byte_cursor();
    init_assert();
    init_constants();
    init_validator();
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/uint.js
function readUintSafe32(bc) {
  let result = readU8(bc);
  if (result >= 128) {
    result &= 127;
    let shift = 7;
    let byteCount = 1;
    let byte;
    do {
      byte = readU8(bc);
      result += (byte & 127) << shift >>> 0;
      shift += 7;
      byteCount++;
    } while (byte >= 128 && byteCount < UINT_SAFE32_MAX_BYTE_COUNT);
    if (byte === 0) {
      bc.offset -= byteCount - 1;
      throw new BareError(bc.offset - byteCount + 1, NON_CANONICAL_REPRESENTATION);
    }
    if (byteCount === UINT_SAFE32_MAX_BYTE_COUNT && byte > 15) {
      bc.offset -= byteCount - 1;
      throw new BareError(bc.offset, TOO_LARGE_NUMBER);
    }
  }
  return result;
}
function writeUintSafe32(bc, x) {
  if (DEV) {
    assert(isU32(x), TOO_LARGE_NUMBER);
  }
  let zigZag = x >>> 0;
  while (zigZag >= 128) {
    writeU8(bc, 128 | zigZag & 127);
    zigZag >>>= 7;
  }
  writeU8(bc, zigZag);
}
function readUintSafe(bc) {
  let result = readU8(bc);
  if (result >= 128) {
    result &= 127;
    let shiftMul = (
      /* 2**7 */
      128
    );
    let byteCount = 1;
    let byte;
    do {
      byte = readU8(bc);
      result += (byte & 127) * shiftMul;
      shiftMul *= /* 2**7 */
      128;
      byteCount++;
    } while (byte >= 128 && byteCount < INT_SAFE_MAX_BYTE_COUNT);
    if (byte === 0) {
      bc.offset -= byteCount - 1;
      throw new BareError(bc.offset - byteCount + 1, NON_CANONICAL_REPRESENTATION);
    }
    if (byteCount === INT_SAFE_MAX_BYTE_COUNT && byte > 15) {
      bc.offset -= byteCount - 1;
      throw new BareError(bc.offset, TOO_LARGE_NUMBER);
    }
  }
  return result;
}
function writeUintSafe(bc, x) {
  if (DEV) {
    assert(isU64Safe(x), TOO_LARGE_NUMBER);
  }
  let byteCount = 1;
  let zigZag = x;
  while (zigZag >= 128 && byteCount < INT_SAFE_MAX_BYTE_COUNT) {
    writeU8(bc, 128 | zigZag & 127);
    zigZag = Math.floor(zigZag / /* 2**7 */
    128);
    byteCount++;
  }
  if (byteCount === INT_SAFE_MAX_BYTE_COUNT) {
    zigZag &= 15;
  }
  writeU8(bc, zigZag);
}
var init_uint = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/uint.js"() {
    init_bare_error();
    init_assert();
    init_constants();
    init_validator();
    init_fixed_primitive();
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u8-array.js
function readU8Array(bc) {
  return readU8FixedArray(bc, readUintSafe32(bc));
}
function writeU8Array(bc, x) {
  writeUintSafe32(bc, x.length);
  writeU8FixedArray(bc, x);
}
function readU8FixedArray(bc, len) {
  return readUnsafeU8FixedArray(bc, len).slice();
}
function writeU8FixedArray(bc, x) {
  const len = x.length;
  if (len > 0) {
    reserve(bc, len);
    bc.bytes.set(x, bc.offset);
    bc.offset += len;
  }
}
function readUnsafeU8FixedArray(bc, len) {
  if (DEV) {
    assert(isU32(len));
  }
  check(bc, len);
  const offset = bc.offset;
  bc.offset += len;
  return bc.bytes.subarray(offset, offset + len);
}
var init_u8_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u8-array.js"() {
    init_byte_cursor();
    init_assert();
    init_validator();
    init_uint();
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/data.js
function readData(bc) {
  return readU8Array(bc).buffer;
}
function writeData(bc, x) {
  writeU8Array(bc, new Uint8Array(x));
}
function readFixedData(bc, len) {
  if (DEV) {
    assert(isU32(len));
  }
  return readU8FixedArray(bc, len).buffer;
}
var init_data = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/data.js"() {
    init_assert();
    init_validator();
    init_u8_array();
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/f32-array.js
var init_f32_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/f32-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/f64-array.js
var init_f64_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/f64-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i8-array.js
var init_i8_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i8-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i16-array.js
var init_i16_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i16-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i32-array.js
var init_i32_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i32-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i64-array.js
var init_i64_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/i64-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/int.js
var init_int = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/int.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/string.js
function readString(bc) {
  return readFixedString(bc, readUintSafe32(bc));
}
function writeString(bc, x) {
  if (x.length < TEXT_ENCODER_THRESHOLD) {
    const byteLen = utf8ByteLength(x);
    writeUintSafe32(bc, byteLen);
    reserve(bc, byteLen);
    writeUtf8Js(bc, x);
  } else {
    const strBytes = UTF8_ENCODER.encode(x);
    writeUintSafe32(bc, strBytes.length);
    writeU8FixedArray(bc, strBytes);
  }
}
function readFixedString(bc, byteLen) {
  if (DEV) {
    assert(isU32(byteLen));
  }
  if (byteLen < TEXT_DECODER_THRESHOLD) {
    return readUtf8Js(bc, byteLen);
  }
  try {
    return UTF8_DECODER.decode(readUnsafeU8FixedArray(bc, byteLen));
  } catch (_cause) {
    throw new BareError(bc.offset, INVALID_UTF8_STRING);
  }
}
function readUtf8Js(bc, byteLen) {
  check(bc, byteLen);
  let result = "";
  const bytes = bc.bytes;
  let offset = bc.offset;
  const upperOffset = offset + byteLen;
  while (offset < upperOffset) {
    let codePoint = bytes[offset++];
    if (codePoint > 127) {
      let malformed = true;
      const byte1 = codePoint;
      if (offset < upperOffset && codePoint < 224) {
        const byte2 = bytes[offset++];
        codePoint = (byte1 & 31) << 6 | byte2 & 63;
        malformed = codePoint >> 7 === 0 || // non-canonical char
        byte1 >> 5 !== 6 || // invalid tag
        byte2 >> 6 !== 2;
      } else if (offset + 1 < upperOffset && codePoint < 240) {
        const byte2 = bytes[offset++];
        const byte3 = bytes[offset++];
        codePoint = (byte1 & 15) << 12 | (byte2 & 63) << 6 | byte3 & 63;
        malformed = codePoint >> 11 === 0 || // non-canonical char or missing data
        codePoint >> 11 === 27 || // surrogate char (0xD800 <= codePoint <= 0xDFFF)
        byte1 >> 4 !== 14 || // invalid tag
        byte2 >> 6 !== 2 || // invalid tag
        byte3 >> 6 !== 2;
      } else if (offset + 2 < upperOffset) {
        const byte2 = bytes[offset++];
        const byte3 = bytes[offset++];
        const byte4 = bytes[offset++];
        codePoint = (byte1 & 7) << 18 | (byte2 & 63) << 12 | (byte3 & 63) << 6 | byte4 & 63;
        malformed = codePoint >> 16 === 0 || // non-canonical char or missing data
        codePoint > 1114111 || // too large code point
        byte1 >> 3 !== 30 || // invalid tag
        byte2 >> 6 !== 2 || // invalid tag
        byte3 >> 6 !== 2 || // invalid tag
        byte4 >> 6 !== 2;
      }
      if (malformed) {
        throw new BareError(bc.offset, INVALID_UTF8_STRING);
      }
    }
    result += String.fromCodePoint(codePoint);
  }
  bc.offset = offset;
  return result;
}
function writeUtf8Js(bc, s) {
  const bytes = bc.bytes;
  let offset = bc.offset;
  let i = 0;
  while (i < s.length) {
    const codePoint = s.codePointAt(i++);
    if (codePoint < 128) {
      bytes[offset++] = codePoint;
    } else {
      if (codePoint < 2048) {
        bytes[offset++] = 192 | codePoint >> 6;
      } else {
        if (codePoint < 65536) {
          bytes[offset++] = 224 | codePoint >> 12;
        } else {
          bytes[offset++] = 240 | codePoint >> 18;
          bytes[offset++] = 128 | codePoint >> 12 & 63;
          i++;
        }
        bytes[offset++] = 128 | codePoint >> 6 & 63;
      }
      bytes[offset++] = 128 | codePoint & 63;
    }
  }
  bc.offset = offset;
}
function utf8ByteLength(s) {
  let result = s.length;
  for (let i = 0; i < s.length; i++) {
    const codePoint = s.codePointAt(i);
    if (codePoint > 127) {
      result++;
      if (codePoint > 2047) {
        result++;
        if (codePoint > 65535) {
          i++;
        }
      }
    }
  }
  return result;
}
var UTF8_DECODER, UTF8_ENCODER;
var init_string = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/string.js"() {
    init_bare_error();
    init_byte_cursor();
    init_assert();
    init_constants();
    init_validator();
    init_u8_array();
    init_uint();
    UTF8_DECODER = /* @__PURE__ */ new TextDecoder("utf-8", { fatal: true });
    UTF8_ENCODER = /* @__PURE__ */ new TextEncoder();
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u8-clamped-array.js
var init_u8_clamped_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u8-clamped-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u16-array.js
function readU16Array(bc) {
  return readU16FixedArray(bc, readUintSafe32(bc));
}
function readU16FixedArrayLe(bc, len) {
  if (DEV) {
    assert(isU32(len));
  }
  const byteCount = len * 2;
  return new Uint16Array(readFixedData(bc, byteCount));
}
function readU16FixedArrayBe(bc, len) {
  if (DEV) {
    assert(isU32(len));
  }
  check(bc, len * 2);
  const result = new Uint16Array(len);
  for (let i = 0; i < len; i++) {
    result[i] = readU16(bc);
  }
  return result;
}
function writeU16Array(bc, x) {
  writeUintSafe32(bc, x.length);
  if (x.length > 0) {
    writeU16FixedArray(bc, x);
  }
}
function writeU16FixedArrayLe(bc, x) {
  writeU8FixedArray(bc, new Uint8Array(x.buffer, x.byteOffset, x.byteLength));
}
function writeU16FixedArrayBe(bc, x) {
  reserve(bc, x.length * 2);
  for (let i = 0; i < x.length; i++) {
    writeU16(bc, x[i]);
  }
}
var readU16FixedArray, writeU16FixedArray;
var init_u16_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u16-array.js"() {
    init_byte_cursor();
    init_assert();
    init_constants();
    init_validator();
    init_data();
    init_fixed_primitive();
    init_u8_array();
    init_uint();
    readU16FixedArray = IS_LITTLE_ENDIAN_PLATFORM ? readU16FixedArrayLe : readU16FixedArrayBe;
    writeU16FixedArray = IS_LITTLE_ENDIAN_PLATFORM ? writeU16FixedArrayLe : writeU16FixedArrayBe;
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u32-array.js
function readU32Array(bc) {
  return readU32FixedArray(bc, readUintSafe32(bc));
}
function readU32FixedArrayLe(bc, len) {
  if (DEV) {
    assert(isU32(len));
  }
  const byteCount = len * 4;
  return new Uint32Array(readFixedData(bc, byteCount));
}
function readU32FixedArrayBe(bc, len) {
  if (DEV) {
    assert(isU32(len));
  }
  check(bc, len * 4);
  const result = new Uint32Array(len);
  for (let i = 0; i < len; i++) {
    result[i] = readU32(bc);
  }
  return result;
}
function writeU32Array(bc, x) {
  writeUintSafe32(bc, x.length);
  if (x.length > 0) {
    writeU32FixedArray(bc, x);
  }
}
function writeU32FixedArrayLe(bc, x) {
  writeU8FixedArray(bc, new Uint8Array(x.buffer, x.byteOffset, x.byteLength));
}
function writeU32FixedArrayBe(bc, x) {
  reserve(bc, x.length * 4);
  for (let i = 0; i < x.length; i++) {
    writeU32(bc, x[i]);
  }
}
var readU32FixedArray, writeU32FixedArray;
var init_u32_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u32-array.js"() {
    init_byte_cursor();
    init_assert();
    init_constants();
    init_validator();
    init_data();
    init_fixed_primitive();
    init_u8_array();
    init_uint();
    readU32FixedArray = IS_LITTLE_ENDIAN_PLATFORM ? readU32FixedArrayLe : readU32FixedArrayBe;
    writeU32FixedArray = IS_LITTLE_ENDIAN_PLATFORM ? writeU32FixedArrayLe : writeU32FixedArrayBe;
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u64-array.js
var init_u64_array = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u64-array.js"() {
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/config.js
function Config({ initialBufferLength = 1024, maxBufferLength = 1024 * 1024 * 32 }) {
  if (DEV) {
    assert(isU32(initialBufferLength), TOO_LARGE_NUMBER);
    assert(isU32(maxBufferLength), TOO_LARGE_NUMBER);
    assert(initialBufferLength <= maxBufferLength, "initialBufferLength must be lower than or equal to maxBufferLength");
  }
  return {
    initialBufferLength,
    maxBufferLength
  };
}
var init_config = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/config.js"() {
    init_assert();
    init_constants();
    init_validator();
  }
});

// ../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/index.js
var init_dist = __esm({
  "../../../agent-os/node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/index.js"() {
    init_data();
    init_f32_array();
    init_f64_array();
    init_fixed_primitive();
    init_i8_array();
    init_i16_array();
    init_i32_array();
    init_i64_array();
    init_int();
    init_string();
    init_u8_array();
    init_u8_clamped_array();
    init_u16_array();
    init_u32_array();
    init_u64_array();
    init_uint();
    init_bare_error();
    init_byte_cursor();
    init_config();
    init_assert();
    init_validator();
  }
});

// ../../../agent-os/packages/core/dist/generated-protocol.js
function readJsonUtf8(bc) {
  return readString(bc);
}
function writeJsonUtf8(bc, x) {
  writeString(bc, x);
}
function readProtocolSchema(bc) {
  return {
    name: readString(bc),
    version: readU16(bc)
  };
}
function writeProtocolSchema(bc, x) {
  writeString(bc, x.name);
  writeU16(bc, x.version);
}
function readRequestId(bc) {
  return readI64(bc);
}
function writeRequestId(bc, x) {
  writeI64(bc, x);
}
function readExtEnvelope(bc) {
  return {
    namespace: readString(bc),
    payload: readData(bc)
  };
}
function writeExtEnvelope(bc, x) {
  writeString(bc, x.namespace);
  writeData(bc, x.payload);
}
function readConnectionOwnership(bc) {
  return {
    connectionId: readString(bc)
  };
}
function writeConnectionOwnership(bc, x) {
  writeString(bc, x.connectionId);
}
function readSessionOwnership(bc) {
  return {
    connectionId: readString(bc),
    sessionId: readString(bc)
  };
}
function writeSessionOwnership(bc, x) {
  writeString(bc, x.connectionId);
  writeString(bc, x.sessionId);
}
function readVmOwnership(bc) {
  return {
    connectionId: readString(bc),
    sessionId: readString(bc),
    vmId: readString(bc)
  };
}
function writeVmOwnership(bc, x) {
  writeString(bc, x.connectionId);
  writeString(bc, x.sessionId);
  writeString(bc, x.vmId);
}
function readOwnershipScope(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "ConnectionOwnership", val: readConnectionOwnership(bc) };
    case 1:
      return { tag: "SessionOwnership", val: readSessionOwnership(bc) };
    case 2:
      return { tag: "VmOwnership", val: readVmOwnership(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeOwnershipScope(bc, x) {
  switch (x.tag) {
    case "ConnectionOwnership": {
      writeU8(bc, 0);
      writeConnectionOwnership(bc, x.val);
      break;
    }
    case "SessionOwnership": {
      writeU8(bc, 1);
      writeSessionOwnership(bc, x.val);
      break;
    }
    case "VmOwnership": {
      writeU8(bc, 2);
      writeVmOwnership(bc, x.val);
      break;
    }
  }
}
function readAuthenticateRequest(bc) {
  return {
    clientName: readString(bc),
    authToken: readString(bc),
    protocolVersion: readU16(bc),
    bridgeVersion: readU32(bc)
  };
}
function writeAuthenticateRequest(bc, x) {
  writeString(bc, x.clientName);
  writeString(bc, x.authToken);
  writeU16(bc, x.protocolVersion);
  writeU32(bc, x.bridgeVersion);
}
function read0(bc) {
  return readBool(bc) ? readString(bc) : null;
}
function write0(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeString(bc, x);
  }
}
function readSidecarPlacementShared(bc) {
  return {
    pool: read0(bc)
  };
}
function writeSidecarPlacementShared(bc, x) {
  write0(bc, x.pool);
}
function readSidecarPlacementExplicit(bc) {
  return {
    sidecarId: readString(bc)
  };
}
function writeSidecarPlacementExplicit(bc, x) {
  writeString(bc, x.sidecarId);
}
function readSidecarPlacement(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "SidecarPlacementShared", val: readSidecarPlacementShared(bc) };
    case 1:
      return { tag: "SidecarPlacementExplicit", val: readSidecarPlacementExplicit(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeSidecarPlacement(bc, x) {
  switch (x.tag) {
    case "SidecarPlacementShared": {
      writeU8(bc, 0);
      writeSidecarPlacementShared(bc, x.val);
      break;
    }
    case "SidecarPlacementExplicit": {
      writeU8(bc, 1);
      writeSidecarPlacementExplicit(bc, x.val);
      break;
    }
  }
}
function read1(bc) {
  const len = readUintSafe(bc);
  const result = /* @__PURE__ */ new Map();
  for (let i = 0; i < len; i++) {
    const offset = bc.offset;
    const key = readString(bc);
    if (result.has(key)) {
      bc.offset = offset;
      throw new BareError(offset, "duplicated key");
    }
    result.set(key, readString(bc));
  }
  return result;
}
function write1(bc, x) {
  writeUintSafe(bc, x.size);
  for (const kv of x) {
    writeString(bc, kv[0]);
    writeString(bc, kv[1]);
  }
}
function readOpenSessionRequest(bc) {
  return {
    placement: readSidecarPlacement(bc),
    metadata: read1(bc)
  };
}
function writeOpenSessionRequest(bc, x) {
  writeSidecarPlacement(bc, x.placement);
  write1(bc, x.metadata);
}
function readGuestRuntimeKind(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return GuestRuntimeKind.JavaScript;
    case 1:
      return GuestRuntimeKind.Python;
    case 2:
      return GuestRuntimeKind.WebAssembly;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeGuestRuntimeKind(bc, x) {
  switch (x) {
    case GuestRuntimeKind.JavaScript: {
      writeU8(bc, 0);
      break;
    }
    case GuestRuntimeKind.Python: {
      writeU8(bc, 1);
      break;
    }
    case GuestRuntimeKind.WebAssembly: {
      writeU8(bc, 2);
      break;
    }
  }
}
function readRootFilesystemMode(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return RootFilesystemMode.Ephemeral;
    case 1:
      return RootFilesystemMode.ReadOnly;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeRootFilesystemMode(bc, x) {
  switch (x) {
    case RootFilesystemMode.Ephemeral: {
      writeU8(bc, 0);
      break;
    }
    case RootFilesystemMode.ReadOnly: {
      writeU8(bc, 1);
      break;
    }
  }
}
function readRootFilesystemEntryKind(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return RootFilesystemEntryKind.File;
    case 1:
      return RootFilesystemEntryKind.Directory;
    case 2:
      return RootFilesystemEntryKind.Symlink;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeRootFilesystemEntryKind(bc, x) {
  switch (x) {
    case RootFilesystemEntryKind.File: {
      writeU8(bc, 0);
      break;
    }
    case RootFilesystemEntryKind.Directory: {
      writeU8(bc, 1);
      break;
    }
    case RootFilesystemEntryKind.Symlink: {
      writeU8(bc, 2);
      break;
    }
  }
}
function readRootFilesystemEntryEncoding(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return RootFilesystemEntryEncoding.UtF8;
    case 1:
      return RootFilesystemEntryEncoding.BasE64;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeRootFilesystemEntryEncoding(bc, x) {
  switch (x) {
    case RootFilesystemEntryEncoding.UtF8: {
      writeU8(bc, 0);
      break;
    }
    case RootFilesystemEntryEncoding.BasE64: {
      writeU8(bc, 1);
      break;
    }
  }
}
function read2(bc) {
  return readBool(bc) ? readU32(bc) : null;
}
function write2(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeU32(bc, x);
  }
}
function read3(bc) {
  return readBool(bc) ? readRootFilesystemEntryEncoding(bc) : null;
}
function write3(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeRootFilesystemEntryEncoding(bc, x);
  }
}
function readRootFilesystemEntry(bc) {
  return {
    path: readString(bc),
    kind: readRootFilesystemEntryKind(bc),
    mode: read2(bc),
    uid: read2(bc),
    gid: read2(bc),
    content: read0(bc),
    encoding: read3(bc),
    target: read0(bc),
    executable: readBool(bc)
  };
}
function writeRootFilesystemEntry(bc, x) {
  writeString(bc, x.path);
  writeRootFilesystemEntryKind(bc, x.kind);
  write2(bc, x.mode);
  write2(bc, x.uid);
  write2(bc, x.gid);
  write0(bc, x.content);
  write3(bc, x.encoding);
  write0(bc, x.target);
  writeBool(bc, x.executable);
}
function read4(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readRootFilesystemEntry(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readRootFilesystemEntry(bc);
  }
  return result;
}
function write4(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeRootFilesystemEntry(bc, x[i]);
  }
}
function readPermissionMode(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return PermissionMode.Allow;
    case 1:
      return PermissionMode.Ask;
    case 2:
      return PermissionMode.Deny;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writePermissionMode(bc, x) {
  switch (x) {
    case PermissionMode.Allow: {
      writeU8(bc, 0);
      break;
    }
    case PermissionMode.Ask: {
      writeU8(bc, 1);
      break;
    }
    case PermissionMode.Deny: {
      writeU8(bc, 2);
      break;
    }
  }
}
function read6(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readString(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readString(bc);
  }
  return result;
}
function write6(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeString(bc, x[i]);
  }
}
function readFsPermissionRule(bc) {
  return {
    mode: readPermissionMode(bc),
    operations: read6(bc),
    paths: read6(bc)
  };
}
function writeFsPermissionRule(bc, x) {
  writePermissionMode(bc, x.mode);
  write6(bc, x.operations);
  write6(bc, x.paths);
}
function read7(bc) {
  return readBool(bc) ? readPermissionMode(bc) : null;
}
function write7(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writePermissionMode(bc, x);
  }
}
function read8(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readFsPermissionRule(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readFsPermissionRule(bc);
  }
  return result;
}
function write8(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeFsPermissionRule(bc, x[i]);
  }
}
function readFsPermissionRuleSet(bc) {
  return {
    default: read7(bc),
    rules: read8(bc)
  };
}
function writeFsPermissionRuleSet(bc, x) {
  write7(bc, x.default);
  write8(bc, x.rules);
}
function readFsPermissionScope(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "PermissionMode", val: readPermissionMode(bc) };
    case 1:
      return { tag: "FsPermissionRuleSet", val: readFsPermissionRuleSet(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeFsPermissionScope(bc, x) {
  switch (x.tag) {
    case "PermissionMode": {
      writeU8(bc, 0);
      writePermissionMode(bc, x.val);
      break;
    }
    case "FsPermissionRuleSet": {
      writeU8(bc, 1);
      writeFsPermissionRuleSet(bc, x.val);
      break;
    }
  }
}
function readPatternPermissionRule(bc) {
  return {
    mode: readPermissionMode(bc),
    operations: read6(bc),
    patterns: read6(bc)
  };
}
function writePatternPermissionRule(bc, x) {
  writePermissionMode(bc, x.mode);
  write6(bc, x.operations);
  write6(bc, x.patterns);
}
function read9(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readPatternPermissionRule(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readPatternPermissionRule(bc);
  }
  return result;
}
function write9(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writePatternPermissionRule(bc, x[i]);
  }
}
function readPatternPermissionRuleSet(bc) {
  return {
    default: read7(bc),
    rules: read9(bc)
  };
}
function writePatternPermissionRuleSet(bc, x) {
  write7(bc, x.default);
  write9(bc, x.rules);
}
function readPatternPermissionScope(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "PermissionMode", val: readPermissionMode(bc) };
    case 1:
      return { tag: "PatternPermissionRuleSet", val: readPatternPermissionRuleSet(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writePatternPermissionScope(bc, x) {
  switch (x.tag) {
    case "PermissionMode": {
      writeU8(bc, 0);
      writePermissionMode(bc, x.val);
      break;
    }
    case "PatternPermissionRuleSet": {
      writeU8(bc, 1);
      writePatternPermissionRuleSet(bc, x.val);
      break;
    }
  }
}
function read10(bc) {
  return readBool(bc) ? readFsPermissionScope(bc) : null;
}
function write10(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeFsPermissionScope(bc, x);
  }
}
function read11(bc) {
  return readBool(bc) ? readPatternPermissionScope(bc) : null;
}
function write11(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writePatternPermissionScope(bc, x);
  }
}
function readPermissionsPolicy(bc) {
  return {
    fs: read10(bc),
    network: read11(bc),
    childProcess: read11(bc),
    process: read11(bc),
    env: read11(bc),
    binding: read11(bc)
  };
}
function writePermissionsPolicy(bc, x) {
  write10(bc, x.fs);
  write11(bc, x.network);
  write11(bc, x.childProcess);
  write11(bc, x.process);
  write11(bc, x.env);
  write11(bc, x.binding);
}
function readCreateVmRequest(bc) {
  return {
    runtime: readGuestRuntimeKind(bc),
    config: readJsonUtf8(bc)
  };
}
function writeCreateVmRequest(bc, x) {
  writeGuestRuntimeKind(bc, x.runtime);
  writeJsonUtf8(bc, x.config);
}
function readDisposeReason(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return DisposeReason.Requested;
    case 1:
      return DisposeReason.ConnectionClosed;
    case 2:
      return DisposeReason.HostShutdown;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeDisposeReason(bc, x) {
  switch (x) {
    case DisposeReason.Requested: {
      writeU8(bc, 0);
      break;
    }
    case DisposeReason.ConnectionClosed: {
      writeU8(bc, 1);
      break;
    }
    case DisposeReason.HostShutdown: {
      writeU8(bc, 2);
      break;
    }
  }
}
function readDisposeVmRequest(bc) {
  return {
    reason: readDisposeReason(bc)
  };
}
function writeDisposeVmRequest(bc, x) {
  writeDisposeReason(bc, x.reason);
}
function readBootstrapRootFilesystemRequest(bc) {
  return {
    entries: read4(bc)
  };
}
function writeBootstrapRootFilesystemRequest(bc, x) {
  write4(bc, x.entries);
}
function readMountPluginDescriptor(bc) {
  return {
    id: readString(bc),
    config: readJsonUtf8(bc)
  };
}
function writeMountPluginDescriptor(bc, x) {
  writeString(bc, x.id);
  writeJsonUtf8(bc, x.config);
}
function readMountDescriptor(bc) {
  return {
    guestPath: readString(bc),
    readOnly: readBool(bc),
    plugin: readMountPluginDescriptor(bc)
  };
}
function writeMountDescriptor(bc, x) {
  writeString(bc, x.guestPath);
  writeBool(bc, x.readOnly);
  writeMountPluginDescriptor(bc, x.plugin);
}
function readSoftwareDescriptor(bc) {
  return {
    packageName: readString(bc),
    root: readString(bc)
  };
}
function writeSoftwareDescriptor(bc, x) {
  writeString(bc, x.packageName);
  writeString(bc, x.root);
}
function readProjectedModuleDescriptor(bc) {
  return {
    packageName: readString(bc),
    entrypoint: readString(bc)
  };
}
function writeProjectedModuleDescriptor(bc, x) {
  writeString(bc, x.packageName);
  writeString(bc, x.entrypoint);
}
function readWasmPermissionTier(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return WasmPermissionTier.Full;
    case 1:
      return WasmPermissionTier.ReadWrite;
    case 2:
      return WasmPermissionTier.ReadOnly;
    case 3:
      return WasmPermissionTier.Isolated;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeWasmPermissionTier(bc, x) {
  switch (x) {
    case WasmPermissionTier.Full: {
      writeU8(bc, 0);
      break;
    }
    case WasmPermissionTier.ReadWrite: {
      writeU8(bc, 1);
      break;
    }
    case WasmPermissionTier.ReadOnly: {
      writeU8(bc, 2);
      break;
    }
    case WasmPermissionTier.Isolated: {
      writeU8(bc, 3);
      break;
    }
  }
}
function read12(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readMountDescriptor(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readMountDescriptor(bc);
  }
  return result;
}
function write12(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeMountDescriptor(bc, x[i]);
  }
}
function read13(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readSoftwareDescriptor(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readSoftwareDescriptor(bc);
  }
  return result;
}
function write13(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeSoftwareDescriptor(bc, x[i]);
  }
}
function read14(bc) {
  return readBool(bc) ? readPermissionsPolicy(bc) : null;
}
function write14(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writePermissionsPolicy(bc, x);
  }
}
function read15(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readProjectedModuleDescriptor(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readProjectedModuleDescriptor(bc);
  }
  return result;
}
function write15(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeProjectedModuleDescriptor(bc, x[i]);
  }
}
function read16(bc) {
  const len = readUintSafe(bc);
  const result = /* @__PURE__ */ new Map();
  for (let i = 0; i < len; i++) {
    const offset = bc.offset;
    const key = readString(bc);
    if (result.has(key)) {
      bc.offset = offset;
      throw new BareError(offset, "duplicated key");
    }
    result.set(key, readWasmPermissionTier(bc));
  }
  return result;
}
function write16(bc, x) {
  writeUintSafe(bc, x.size);
  for (const kv of x) {
    writeString(bc, kv[0]);
    writeWasmPermissionTier(bc, kv[1]);
  }
}
function readConfigureVmRequest(bc) {
  return {
    mounts: read12(bc),
    software: read13(bc),
    permissions: read14(bc),
    moduleAccessCwd: read0(bc),
    instructions: read6(bc),
    projectedModules: read15(bc),
    commandPermissions: read16(bc),
    loopbackExemptPorts: readU16Array(bc)
  };
}
function writeConfigureVmRequest(bc, x) {
  write12(bc, x.mounts);
  write13(bc, x.software);
  write14(bc, x.permissions);
  write0(bc, x.moduleAccessCwd);
  write6(bc, x.instructions);
  write15(bc, x.projectedModules);
  write16(bc, x.commandPermissions);
  writeU16Array(bc, x.loopbackExemptPorts);
}
function readRegisteredHostCallbackExample(bc) {
  return {
    description: readString(bc),
    input: readJsonUtf8(bc)
  };
}
function writeRegisteredHostCallbackExample(bc, x) {
  writeString(bc, x.description);
  writeJsonUtf8(bc, x.input);
}
function read17(bc) {
  return readBool(bc) ? readU64(bc) : null;
}
function write17(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeU64(bc, x);
  }
}
function read18(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readRegisteredHostCallbackExample(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readRegisteredHostCallbackExample(bc);
  }
  return result;
}
function write18(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeRegisteredHostCallbackExample(bc, x[i]);
  }
}
function readRegisteredHostCallbackDefinition(bc) {
  return {
    description: readString(bc),
    inputSchema: readJsonUtf8(bc),
    timeoutMs: read17(bc),
    examples: read18(bc)
  };
}
function writeRegisteredHostCallbackDefinition(bc, x) {
  writeString(bc, x.description);
  writeJsonUtf8(bc, x.inputSchema);
  write17(bc, x.timeoutMs);
  write18(bc, x.examples);
}
function read19(bc) {
  const len = readUintSafe(bc);
  const result = /* @__PURE__ */ new Map();
  for (let i = 0; i < len; i++) {
    const offset = bc.offset;
    const key = readString(bc);
    if (result.has(key)) {
      bc.offset = offset;
      throw new BareError(offset, "duplicated key");
    }
    result.set(key, readRegisteredHostCallbackDefinition(bc));
  }
  return result;
}
function write19(bc, x) {
  writeUintSafe(bc, x.size);
  for (const kv of x) {
    writeString(bc, kv[0]);
    writeRegisteredHostCallbackDefinition(bc, kv[1]);
  }
}
function readRegisterHostCallbacksRequest(bc) {
  return {
    name: readString(bc),
    description: readString(bc),
    commandAliases: read6(bc),
    registryCommandAliases: read6(bc),
    callbacks: read19(bc)
  };
}
function writeRegisterHostCallbacksRequest(bc, x) {
  writeString(bc, x.name);
  writeString(bc, x.description);
  write6(bc, x.commandAliases);
  write6(bc, x.registryCommandAliases);
  write19(bc, x.callbacks);
}
function readSealLayerRequest(bc) {
  return {
    layerId: readString(bc)
  };
}
function writeSealLayerRequest(bc, x) {
  writeString(bc, x.layerId);
}
function readImportSnapshotRequest(bc) {
  return {
    entries: read4(bc)
  };
}
function writeImportSnapshotRequest(bc, x) {
  write4(bc, x.entries);
}
function readExportSnapshotRequest(bc) {
  return {
    layerId: readString(bc)
  };
}
function writeExportSnapshotRequest(bc, x) {
  writeString(bc, x.layerId);
}
function readCreateOverlayRequest(bc) {
  return {
    mode: readRootFilesystemMode(bc),
    upperLayerId: read0(bc),
    lowerLayerIds: read6(bc)
  };
}
function writeCreateOverlayRequest(bc, x) {
  writeRootFilesystemMode(bc, x.mode);
  write0(bc, x.upperLayerId);
  write6(bc, x.lowerLayerIds);
}
function readGuestFilesystemOperation(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return GuestFilesystemOperation.ReadFile;
    case 1:
      return GuestFilesystemOperation.WriteFile;
    case 2:
      return GuestFilesystemOperation.CreateDir;
    case 3:
      return GuestFilesystemOperation.Mkdir;
    case 4:
      return GuestFilesystemOperation.Exists;
    case 5:
      return GuestFilesystemOperation.Stat;
    case 6:
      return GuestFilesystemOperation.Lstat;
    case 7:
      return GuestFilesystemOperation.ReadDir;
    case 8:
      return GuestFilesystemOperation.RemoveFile;
    case 9:
      return GuestFilesystemOperation.RemoveDir;
    case 10:
      return GuestFilesystemOperation.Rename;
    case 11:
      return GuestFilesystemOperation.Realpath;
    case 12:
      return GuestFilesystemOperation.Symlink;
    case 13:
      return GuestFilesystemOperation.ReadLink;
    case 14:
      return GuestFilesystemOperation.Link;
    case 15:
      return GuestFilesystemOperation.Chmod;
    case 16:
      return GuestFilesystemOperation.Chown;
    case 17:
      return GuestFilesystemOperation.Utimes;
    case 18:
      return GuestFilesystemOperation.Truncate;
    case 19:
      return GuestFilesystemOperation.Pread;
    case 20:
      return GuestFilesystemOperation.Pwrite;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeGuestFilesystemOperation(bc, x) {
  switch (x) {
    case GuestFilesystemOperation.ReadFile: {
      writeU8(bc, 0);
      break;
    }
    case GuestFilesystemOperation.WriteFile: {
      writeU8(bc, 1);
      break;
    }
    case GuestFilesystemOperation.CreateDir: {
      writeU8(bc, 2);
      break;
    }
    case GuestFilesystemOperation.Mkdir: {
      writeU8(bc, 3);
      break;
    }
    case GuestFilesystemOperation.Exists: {
      writeU8(bc, 4);
      break;
    }
    case GuestFilesystemOperation.Stat: {
      writeU8(bc, 5);
      break;
    }
    case GuestFilesystemOperation.Lstat: {
      writeU8(bc, 6);
      break;
    }
    case GuestFilesystemOperation.ReadDir: {
      writeU8(bc, 7);
      break;
    }
    case GuestFilesystemOperation.RemoveFile: {
      writeU8(bc, 8);
      break;
    }
    case GuestFilesystemOperation.RemoveDir: {
      writeU8(bc, 9);
      break;
    }
    case GuestFilesystemOperation.Rename: {
      writeU8(bc, 10);
      break;
    }
    case GuestFilesystemOperation.Realpath: {
      writeU8(bc, 11);
      break;
    }
    case GuestFilesystemOperation.Symlink: {
      writeU8(bc, 12);
      break;
    }
    case GuestFilesystemOperation.ReadLink: {
      writeU8(bc, 13);
      break;
    }
    case GuestFilesystemOperation.Link: {
      writeU8(bc, 14);
      break;
    }
    case GuestFilesystemOperation.Chmod: {
      writeU8(bc, 15);
      break;
    }
    case GuestFilesystemOperation.Chown: {
      writeU8(bc, 16);
      break;
    }
    case GuestFilesystemOperation.Utimes: {
      writeU8(bc, 17);
      break;
    }
    case GuestFilesystemOperation.Truncate: {
      writeU8(bc, 18);
      break;
    }
    case GuestFilesystemOperation.Pread: {
      writeU8(bc, 19);
      break;
    }
    case GuestFilesystemOperation.Pwrite: {
      writeU8(bc, 20);
      break;
    }
  }
}
function readGuestFilesystemCallRequest(bc) {
  return {
    operation: readGuestFilesystemOperation(bc),
    path: readString(bc),
    destinationPath: read0(bc),
    target: read0(bc),
    content: read0(bc),
    encoding: read3(bc),
    recursive: readBool(bc),
    mode: read2(bc),
    uid: read2(bc),
    gid: read2(bc),
    atimeMs: read17(bc),
    mtimeMs: read17(bc),
    len: read17(bc),
    offset: read17(bc)
  };
}
function writeGuestFilesystemCallRequest(bc, x) {
  writeGuestFilesystemOperation(bc, x.operation);
  writeString(bc, x.path);
  write0(bc, x.destinationPath);
  write0(bc, x.target);
  write0(bc, x.content);
  write3(bc, x.encoding);
  writeBool(bc, x.recursive);
  write2(bc, x.mode);
  write2(bc, x.uid);
  write2(bc, x.gid);
  write17(bc, x.atimeMs);
  write17(bc, x.mtimeMs);
  write17(bc, x.len);
  write17(bc, x.offset);
}
function readGuestKernelCallRequest(bc) {
  return {
    executionId: readString(bc),
    operation: readString(bc),
    payload: readData(bc)
  };
}
function writeGuestKernelCallRequest(bc, x) {
  writeString(bc, x.executionId);
  writeString(bc, x.operation);
  writeData(bc, x.payload);
}
function read20(bc) {
  return readBool(bc) ? readGuestRuntimeKind(bc) : null;
}
function write20(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeGuestRuntimeKind(bc, x);
  }
}
function read21(bc) {
  return readBool(bc) ? readWasmPermissionTier(bc) : null;
}
function write21(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeWasmPermissionTier(bc, x);
  }
}
function readExecuteRequest(bc) {
  return {
    processId: readString(bc),
    command: read0(bc),
    runtime: read20(bc),
    entrypoint: read0(bc),
    args: read6(bc),
    env: read1(bc),
    cwd: read0(bc),
    wasmPermissionTier: read21(bc)
  };
}
function writeExecuteRequest(bc, x) {
  writeString(bc, x.processId);
  write0(bc, x.command);
  write20(bc, x.runtime);
  write0(bc, x.entrypoint);
  write6(bc, x.args);
  write1(bc, x.env);
  write0(bc, x.cwd);
  write21(bc, x.wasmPermissionTier);
}
function readWriteStdinRequest(bc) {
  return {
    processId: readString(bc),
    chunk: readData(bc)
  };
}
function writeWriteStdinRequest(bc, x) {
  writeString(bc, x.processId);
  writeData(bc, x.chunk);
}
function readResizePtyRequest(bc) {
  return {
    processId: readString(bc),
    cols: readU16(bc),
    rows: readU16(bc)
  };
}
function writeResizePtyRequest(bc, x) {
  writeString(bc, x.processId);
  writeU16(bc, x.cols);
  writeU16(bc, x.rows);
}
function readCloseStdinRequest(bc) {
  return {
    processId: readString(bc)
  };
}
function writeCloseStdinRequest(bc, x) {
  writeString(bc, x.processId);
}
function readKillProcessRequest(bc) {
  return {
    processId: readString(bc),
    signal: readString(bc)
  };
}
function writeKillProcessRequest(bc, x) {
  writeString(bc, x.processId);
  writeString(bc, x.signal);
}
function read22(bc) {
  return readBool(bc) ? readU16(bc) : null;
}
function write22(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeU16(bc, x);
  }
}
function readFindListenerRequest(bc) {
  return {
    host: read0(bc),
    port: read22(bc),
    path: read0(bc)
  };
}
function writeFindListenerRequest(bc, x) {
  write0(bc, x.host);
  write22(bc, x.port);
  write0(bc, x.path);
}
function readFindBoundUdpRequest(bc) {
  return {
    host: read0(bc),
    port: read22(bc)
  };
}
function writeFindBoundUdpRequest(bc, x) {
  write0(bc, x.host);
  write22(bc, x.port);
}
function readGetSignalStateRequest(bc) {
  return {
    processId: readString(bc)
  };
}
function writeGetSignalStateRequest(bc, x) {
  writeString(bc, x.processId);
}
function readFilesystemOperation(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return FilesystemOperation.Read;
    case 1:
      return FilesystemOperation.Write;
    case 2:
      return FilesystemOperation.Stat;
    case 3:
      return FilesystemOperation.ReadDir;
    case 4:
      return FilesystemOperation.Mkdir;
    case 5:
      return FilesystemOperation.Remove;
    case 6:
      return FilesystemOperation.Rename;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeFilesystemOperation(bc, x) {
  switch (x) {
    case FilesystemOperation.Read: {
      writeU8(bc, 0);
      break;
    }
    case FilesystemOperation.Write: {
      writeU8(bc, 1);
      break;
    }
    case FilesystemOperation.Stat: {
      writeU8(bc, 2);
      break;
    }
    case FilesystemOperation.ReadDir: {
      writeU8(bc, 3);
      break;
    }
    case FilesystemOperation.Mkdir: {
      writeU8(bc, 4);
      break;
    }
    case FilesystemOperation.Remove: {
      writeU8(bc, 5);
      break;
    }
    case FilesystemOperation.Rename: {
      writeU8(bc, 6);
      break;
    }
  }
}
function readHostFilesystemCallRequest(bc) {
  return {
    operation: readFilesystemOperation(bc),
    path: readString(bc),
    payloadSizeBytes: readU64(bc)
  };
}
function writeHostFilesystemCallRequest(bc, x) {
  writeFilesystemOperation(bc, x.operation);
  writeString(bc, x.path);
  writeU64(bc, x.payloadSizeBytes);
}
function readPersistenceLoadRequest(bc) {
  return {
    key: readString(bc)
  };
}
function writePersistenceLoadRequest(bc, x) {
  writeString(bc, x.key);
}
function readPersistenceFlushRequest(bc) {
  return {
    key: readString(bc),
    payloadSizeBytes: readU64(bc)
  };
}
function writePersistenceFlushRequest(bc, x) {
  writeString(bc, x.key);
  writeU64(bc, x.payloadSizeBytes);
}
function readVmFetchRequest(bc) {
  return {
    port: readU16(bc),
    method: readString(bc),
    path: readString(bc),
    headersJson: readString(bc),
    body: read0(bc)
  };
}
function writeVmFetchRequest(bc, x) {
  writeU16(bc, x.port);
  writeString(bc, x.method);
  writeString(bc, x.path);
  writeString(bc, x.headersJson);
  write0(bc, x.body);
}
function readRequestPayload(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "AuthenticateRequest", val: readAuthenticateRequest(bc) };
    case 1:
      return { tag: "OpenSessionRequest", val: readOpenSessionRequest(bc) };
    case 2:
      return { tag: "CreateVmRequest", val: readCreateVmRequest(bc) };
    case 3:
      return { tag: "DisposeVmRequest", val: readDisposeVmRequest(bc) };
    case 4:
      return { tag: "BootstrapRootFilesystemRequest", val: readBootstrapRootFilesystemRequest(bc) };
    case 5:
      return { tag: "ConfigureVmRequest", val: readConfigureVmRequest(bc) };
    case 6:
      return { tag: "RegisterHostCallbacksRequest", val: readRegisterHostCallbacksRequest(bc) };
    case 7:
      return { tag: "CreateLayerRequest", val: null };
    case 8:
      return { tag: "SealLayerRequest", val: readSealLayerRequest(bc) };
    case 9:
      return { tag: "ImportSnapshotRequest", val: readImportSnapshotRequest(bc) };
    case 10:
      return { tag: "ExportSnapshotRequest", val: readExportSnapshotRequest(bc) };
    case 11:
      return { tag: "CreateOverlayRequest", val: readCreateOverlayRequest(bc) };
    case 12:
      return { tag: "GuestFilesystemCallRequest", val: readGuestFilesystemCallRequest(bc) };
    case 13:
      return { tag: "SnapshotRootFilesystemRequest", val: null };
    case 14:
      return { tag: "ExecuteRequest", val: readExecuteRequest(bc) };
    case 15:
      return { tag: "WriteStdinRequest", val: readWriteStdinRequest(bc) };
    case 16:
      return { tag: "CloseStdinRequest", val: readCloseStdinRequest(bc) };
    case 17:
      return { tag: "KillProcessRequest", val: readKillProcessRequest(bc) };
    case 18:
      return { tag: "GetProcessSnapshotRequest", val: null };
    case 19:
      return { tag: "FindListenerRequest", val: readFindListenerRequest(bc) };
    case 20:
      return { tag: "FindBoundUdpRequest", val: readFindBoundUdpRequest(bc) };
    case 21:
      return { tag: "GetSignalStateRequest", val: readGetSignalStateRequest(bc) };
    case 22:
      return { tag: "GetZombieTimerCountRequest", val: null };
    case 23:
      return { tag: "HostFilesystemCallRequest", val: readHostFilesystemCallRequest(bc) };
    case 24:
      return { tag: "PersistenceLoadRequest", val: readPersistenceLoadRequest(bc) };
    case 25:
      return { tag: "PersistenceFlushRequest", val: readPersistenceFlushRequest(bc) };
    case 26:
      return { tag: "VmFetchRequest", val: readVmFetchRequest(bc) };
    case 27:
      return { tag: "ExtEnvelope", val: readExtEnvelope(bc) };
    case 28:
      return { tag: "GuestKernelCallRequest", val: readGuestKernelCallRequest(bc) };
    case 29:
      return { tag: "ResizePtyRequest", val: readResizePtyRequest(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeRequestPayload(bc, x) {
  switch (x.tag) {
    case "AuthenticateRequest": {
      writeU8(bc, 0);
      writeAuthenticateRequest(bc, x.val);
      break;
    }
    case "OpenSessionRequest": {
      writeU8(bc, 1);
      writeOpenSessionRequest(bc, x.val);
      break;
    }
    case "CreateVmRequest": {
      writeU8(bc, 2);
      writeCreateVmRequest(bc, x.val);
      break;
    }
    case "DisposeVmRequest": {
      writeU8(bc, 3);
      writeDisposeVmRequest(bc, x.val);
      break;
    }
    case "BootstrapRootFilesystemRequest": {
      writeU8(bc, 4);
      writeBootstrapRootFilesystemRequest(bc, x.val);
      break;
    }
    case "ConfigureVmRequest": {
      writeU8(bc, 5);
      writeConfigureVmRequest(bc, x.val);
      break;
    }
    case "RegisterHostCallbacksRequest": {
      writeU8(bc, 6);
      writeRegisterHostCallbacksRequest(bc, x.val);
      break;
    }
    case "CreateLayerRequest": {
      writeU8(bc, 7);
      break;
    }
    case "SealLayerRequest": {
      writeU8(bc, 8);
      writeSealLayerRequest(bc, x.val);
      break;
    }
    case "ImportSnapshotRequest": {
      writeU8(bc, 9);
      writeImportSnapshotRequest(bc, x.val);
      break;
    }
    case "ExportSnapshotRequest": {
      writeU8(bc, 10);
      writeExportSnapshotRequest(bc, x.val);
      break;
    }
    case "CreateOverlayRequest": {
      writeU8(bc, 11);
      writeCreateOverlayRequest(bc, x.val);
      break;
    }
    case "GuestFilesystemCallRequest": {
      writeU8(bc, 12);
      writeGuestFilesystemCallRequest(bc, x.val);
      break;
    }
    case "SnapshotRootFilesystemRequest": {
      writeU8(bc, 13);
      break;
    }
    case "ExecuteRequest": {
      writeU8(bc, 14);
      writeExecuteRequest(bc, x.val);
      break;
    }
    case "WriteStdinRequest": {
      writeU8(bc, 15);
      writeWriteStdinRequest(bc, x.val);
      break;
    }
    case "CloseStdinRequest": {
      writeU8(bc, 16);
      writeCloseStdinRequest(bc, x.val);
      break;
    }
    case "KillProcessRequest": {
      writeU8(bc, 17);
      writeKillProcessRequest(bc, x.val);
      break;
    }
    case "GetProcessSnapshotRequest": {
      writeU8(bc, 18);
      break;
    }
    case "FindListenerRequest": {
      writeU8(bc, 19);
      writeFindListenerRequest(bc, x.val);
      break;
    }
    case "FindBoundUdpRequest": {
      writeU8(bc, 20);
      writeFindBoundUdpRequest(bc, x.val);
      break;
    }
    case "GetSignalStateRequest": {
      writeU8(bc, 21);
      writeGetSignalStateRequest(bc, x.val);
      break;
    }
    case "GetZombieTimerCountRequest": {
      writeU8(bc, 22);
      break;
    }
    case "HostFilesystemCallRequest": {
      writeU8(bc, 23);
      writeHostFilesystemCallRequest(bc, x.val);
      break;
    }
    case "PersistenceLoadRequest": {
      writeU8(bc, 24);
      writePersistenceLoadRequest(bc, x.val);
      break;
    }
    case "PersistenceFlushRequest": {
      writeU8(bc, 25);
      writePersistenceFlushRequest(bc, x.val);
      break;
    }
    case "VmFetchRequest": {
      writeU8(bc, 26);
      writeVmFetchRequest(bc, x.val);
      break;
    }
    case "ExtEnvelope": {
      writeU8(bc, 27);
      writeExtEnvelope(bc, x.val);
      break;
    }
    case "GuestKernelCallRequest": {
      writeU8(bc, 28);
      writeGuestKernelCallRequest(bc, x.val);
      break;
    }
    case "ResizePtyRequest": {
      writeU8(bc, 29);
      writeResizePtyRequest(bc, x.val);
      break;
    }
  }
}
function readRequestFrame(bc) {
  return {
    schema: readProtocolSchema(bc),
    requestId: readRequestId(bc),
    ownership: readOwnershipScope(bc),
    payload: readRequestPayload(bc)
  };
}
function writeRequestFrame(bc, x) {
  writeProtocolSchema(bc, x.schema);
  writeRequestId(bc, x.requestId);
  writeOwnershipScope(bc, x.ownership);
  writeRequestPayload(bc, x.payload);
}
function readAuthenticatedResponse(bc) {
  return {
    sidecarId: readString(bc),
    connectionId: readString(bc),
    maxFrameBytes: readU32(bc)
  };
}
function writeAuthenticatedResponse(bc, x) {
  writeString(bc, x.sidecarId);
  writeString(bc, x.connectionId);
  writeU32(bc, x.maxFrameBytes);
}
function readSessionOpenedResponse(bc) {
  return {
    sessionId: readString(bc),
    ownerConnectionId: readString(bc)
  };
}
function writeSessionOpenedResponse(bc, x) {
  writeString(bc, x.sessionId);
  writeString(bc, x.ownerConnectionId);
}
function readVmCreatedResponse(bc) {
  return {
    vmId: readString(bc)
  };
}
function writeVmCreatedResponse(bc, x) {
  writeString(bc, x.vmId);
}
function readVmDisposedResponse(bc) {
  return {
    vmId: readString(bc)
  };
}
function writeVmDisposedResponse(bc, x) {
  writeString(bc, x.vmId);
}
function readRootFilesystemBootstrappedResponse(bc) {
  return {
    entryCount: readU32(bc)
  };
}
function writeRootFilesystemBootstrappedResponse(bc, x) {
  writeU32(bc, x.entryCount);
}
function readVmConfiguredResponse(bc) {
  return {
    appliedMounts: readU32(bc),
    appliedSoftware: readU32(bc)
  };
}
function writeVmConfiguredResponse(bc, x) {
  writeU32(bc, x.appliedMounts);
  writeU32(bc, x.appliedSoftware);
}
function readHostCallbacksRegisteredResponse(bc) {
  return {
    registration: readString(bc),
    commandCount: readU32(bc)
  };
}
function writeHostCallbacksRegisteredResponse(bc, x) {
  writeString(bc, x.registration);
  writeU32(bc, x.commandCount);
}
function readLayerCreatedResponse(bc) {
  return {
    layerId: readString(bc)
  };
}
function writeLayerCreatedResponse(bc, x) {
  writeString(bc, x.layerId);
}
function readLayerSealedResponse(bc) {
  return {
    layerId: readString(bc)
  };
}
function writeLayerSealedResponse(bc, x) {
  writeString(bc, x.layerId);
}
function readSnapshotImportedResponse(bc) {
  return {
    layerId: readString(bc)
  };
}
function writeSnapshotImportedResponse(bc, x) {
  writeString(bc, x.layerId);
}
function readSnapshotExportedResponse(bc) {
  return {
    layerId: readString(bc),
    entries: read4(bc)
  };
}
function writeSnapshotExportedResponse(bc, x) {
  writeString(bc, x.layerId);
  write4(bc, x.entries);
}
function readOverlayCreatedResponse(bc) {
  return {
    layerId: readString(bc)
  };
}
function writeOverlayCreatedResponse(bc, x) {
  writeString(bc, x.layerId);
}
function readGuestFilesystemStat(bc) {
  return {
    mode: readU32(bc),
    size: readU64(bc),
    blocks: readU64(bc),
    dev: readU64(bc),
    rdev: readU64(bc),
    isDirectory: readBool(bc),
    isSymbolicLink: readBool(bc),
    atimeMs: readU64(bc),
    mtimeMs: readU64(bc),
    ctimeMs: readU64(bc),
    birthtimeMs: readU64(bc),
    ino: readU64(bc),
    nlink: readU64(bc),
    uid: readU32(bc),
    gid: readU32(bc)
  };
}
function writeGuestFilesystemStat(bc, x) {
  writeU32(bc, x.mode);
  writeU64(bc, x.size);
  writeU64(bc, x.blocks);
  writeU64(bc, x.dev);
  writeU64(bc, x.rdev);
  writeBool(bc, x.isDirectory);
  writeBool(bc, x.isSymbolicLink);
  writeU64(bc, x.atimeMs);
  writeU64(bc, x.mtimeMs);
  writeU64(bc, x.ctimeMs);
  writeU64(bc, x.birthtimeMs);
  writeU64(bc, x.ino);
  writeU64(bc, x.nlink);
  writeU32(bc, x.uid);
  writeU32(bc, x.gid);
}
function readGuestDirEntry(bc) {
  return {
    name: readString(bc),
    isDirectory: readBool(bc),
    isSymbolicLink: readBool(bc)
  };
}
function writeGuestDirEntry(bc, x) {
  writeString(bc, x.name);
  writeBool(bc, x.isDirectory);
  writeBool(bc, x.isSymbolicLink);
}
function read23(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readGuestDirEntry(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readGuestDirEntry(bc);
  }
  return result;
}
function write23(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeGuestDirEntry(bc, x[i]);
  }
}
function read24(bc) {
  return readBool(bc) ? read23(bc) : null;
}
function write24(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    write23(bc, x);
  }
}
function read25(bc) {
  return readBool(bc) ? readGuestFilesystemStat(bc) : null;
}
function write25(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeGuestFilesystemStat(bc, x);
  }
}
function read26(bc) {
  return readBool(bc) ? readBool(bc) : null;
}
function write26(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeBool(bc, x);
  }
}
function readGuestFilesystemResultResponse(bc) {
  return {
    operation: readGuestFilesystemOperation(bc),
    path: readString(bc),
    content: read0(bc),
    encoding: read3(bc),
    entries: read24(bc),
    stat: read25(bc),
    exists: read26(bc),
    target: read0(bc)
  };
}
function writeGuestFilesystemResultResponse(bc, x) {
  writeGuestFilesystemOperation(bc, x.operation);
  writeString(bc, x.path);
  write0(bc, x.content);
  write3(bc, x.encoding);
  write24(bc, x.entries);
  write25(bc, x.stat);
  write26(bc, x.exists);
  write0(bc, x.target);
}
function readGuestKernelResultResponse(bc) {
  return {
    payload: readData(bc)
  };
}
function writeGuestKernelResultResponse(bc, x) {
  writeData(bc, x.payload);
}
function readRootFilesystemSnapshotResponse(bc) {
  return {
    entries: read4(bc)
  };
}
function writeRootFilesystemSnapshotResponse(bc, x) {
  write4(bc, x.entries);
}
function readProcessStartedResponse(bc) {
  return {
    processId: readString(bc),
    pid: read2(bc)
  };
}
function writeProcessStartedResponse(bc, x) {
  writeString(bc, x.processId);
  write2(bc, x.pid);
}
function readStdinWrittenResponse(bc) {
  return {
    processId: readString(bc),
    acceptedBytes: readU64(bc)
  };
}
function writeStdinWrittenResponse(bc, x) {
  writeString(bc, x.processId);
  writeU64(bc, x.acceptedBytes);
}
function readPtyResizedResponse(bc) {
  return {
    processId: readString(bc),
    cols: readU16(bc),
    rows: readU16(bc)
  };
}
function writePtyResizedResponse(bc, x) {
  writeString(bc, x.processId);
  writeU16(bc, x.cols);
  writeU16(bc, x.rows);
}
function readStdinClosedResponse(bc) {
  return {
    processId: readString(bc)
  };
}
function writeStdinClosedResponse(bc, x) {
  writeString(bc, x.processId);
}
function readProcessKilledResponse(bc) {
  return {
    processId: readString(bc)
  };
}
function writeProcessKilledResponse(bc, x) {
  writeString(bc, x.processId);
}
function readProcessSnapshotStatus(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return ProcessSnapshotStatus.Running;
    case 1:
      return ProcessSnapshotStatus.Exited;
    case 2:
      return ProcessSnapshotStatus.Stopped;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeProcessSnapshotStatus(bc, x) {
  switch (x) {
    case ProcessSnapshotStatus.Running: {
      writeU8(bc, 0);
      break;
    }
    case ProcessSnapshotStatus.Exited: {
      writeU8(bc, 1);
      break;
    }
    case ProcessSnapshotStatus.Stopped: {
      writeU8(bc, 2);
      break;
    }
  }
}
function read27(bc) {
  return readBool(bc) ? readI32(bc) : null;
}
function write27(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeI32(bc, x);
  }
}
function readProcessSnapshotEntry(bc) {
  return {
    processId: readString(bc),
    pid: readU32(bc),
    ppid: readU32(bc),
    pgid: readU32(bc),
    sid: readU32(bc),
    driver: readString(bc),
    command: readString(bc),
    args: read6(bc),
    cwd: readString(bc),
    status: readProcessSnapshotStatus(bc),
    exitCode: read27(bc)
  };
}
function writeProcessSnapshotEntry(bc, x) {
  writeString(bc, x.processId);
  writeU32(bc, x.pid);
  writeU32(bc, x.ppid);
  writeU32(bc, x.pgid);
  writeU32(bc, x.sid);
  writeString(bc, x.driver);
  writeString(bc, x.command);
  write6(bc, x.args);
  writeString(bc, x.cwd);
  writeProcessSnapshotStatus(bc, x.status);
  write27(bc, x.exitCode);
}
function read28(bc) {
  const len = readUintSafe(bc);
  if (len === 0) {
    return [];
  }
  const result = [readProcessSnapshotEntry(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readProcessSnapshotEntry(bc);
  }
  return result;
}
function write28(bc, x) {
  writeUintSafe(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeProcessSnapshotEntry(bc, x[i]);
  }
}
function readProcessSnapshotResponse(bc) {
  return {
    processes: read28(bc)
  };
}
function writeProcessSnapshotResponse(bc, x) {
  write28(bc, x.processes);
}
function readSocketStateEntry(bc) {
  return {
    processId: readString(bc),
    host: read0(bc),
    port: read22(bc),
    path: read0(bc)
  };
}
function writeSocketStateEntry(bc, x) {
  writeString(bc, x.processId);
  write0(bc, x.host);
  write22(bc, x.port);
  write0(bc, x.path);
}
function read29(bc) {
  return readBool(bc) ? readSocketStateEntry(bc) : null;
}
function write29(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeSocketStateEntry(bc, x);
  }
}
function readListenerSnapshotResponse(bc) {
  return {
    listener: read29(bc)
  };
}
function writeListenerSnapshotResponse(bc, x) {
  write29(bc, x.listener);
}
function readBoundUdpSnapshotResponse(bc) {
  return {
    socket: read29(bc)
  };
}
function writeBoundUdpSnapshotResponse(bc, x) {
  write29(bc, x.socket);
}
function readSignalDispositionAction(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return SignalDispositionAction.Default;
    case 1:
      return SignalDispositionAction.Ignore;
    case 2:
      return SignalDispositionAction.User;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeSignalDispositionAction(bc, x) {
  switch (x) {
    case SignalDispositionAction.Default: {
      writeU8(bc, 0);
      break;
    }
    case SignalDispositionAction.Ignore: {
      writeU8(bc, 1);
      break;
    }
    case SignalDispositionAction.User: {
      writeU8(bc, 2);
      break;
    }
  }
}
function readSignalHandlerRegistration(bc) {
  return {
    action: readSignalDispositionAction(bc),
    mask: readU32Array(bc),
    flags: readU32(bc)
  };
}
function writeSignalHandlerRegistration(bc, x) {
  writeSignalDispositionAction(bc, x.action);
  writeU32Array(bc, x.mask);
  writeU32(bc, x.flags);
}
function read30(bc) {
  const len = readUintSafe(bc);
  const result = /* @__PURE__ */ new Map();
  for (let i = 0; i < len; i++) {
    const offset = bc.offset;
    const key = readU32(bc);
    if (result.has(key)) {
      bc.offset = offset;
      throw new BareError(offset, "duplicated key");
    }
    result.set(key, readSignalHandlerRegistration(bc));
  }
  return result;
}
function write30(bc, x) {
  writeUintSafe(bc, x.size);
  for (const kv of x) {
    writeU32(bc, kv[0]);
    writeSignalHandlerRegistration(bc, kv[1]);
  }
}
function readSignalStateResponse(bc) {
  return {
    processId: readString(bc),
    handlers: read30(bc)
  };
}
function writeSignalStateResponse(bc, x) {
  writeString(bc, x.processId);
  write30(bc, x.handlers);
}
function readZombieTimerCountResponse(bc) {
  return {
    count: readU64(bc)
  };
}
function writeZombieTimerCountResponse(bc, x) {
  writeU64(bc, x.count);
}
function readFilesystemResultResponse(bc) {
  return {
    operation: readFilesystemOperation(bc),
    status: readString(bc),
    payloadSizeBytes: readU64(bc)
  };
}
function writeFilesystemResultResponse(bc, x) {
  writeFilesystemOperation(bc, x.operation);
  writeString(bc, x.status);
  writeU64(bc, x.payloadSizeBytes);
}
function readPermissionDecisionResponse(bc) {
  return {
    capability: readString(bc),
    decision: readPermissionMode(bc)
  };
}
function writePermissionDecisionResponse(bc, x) {
  writeString(bc, x.capability);
  writePermissionMode(bc, x.decision);
}
function readPersistenceStateResponse(bc) {
  return {
    key: readString(bc),
    found: readBool(bc),
    payloadSizeBytes: readU64(bc)
  };
}
function writePersistenceStateResponse(bc, x) {
  writeString(bc, x.key);
  writeBool(bc, x.found);
  writeU64(bc, x.payloadSizeBytes);
}
function readPersistenceFlushedResponse(bc) {
  return {
    key: readString(bc),
    committedBytes: readU64(bc)
  };
}
function writePersistenceFlushedResponse(bc, x) {
  writeString(bc, x.key);
  writeU64(bc, x.committedBytes);
}
function readRejectedResponse(bc) {
  return {
    code: readString(bc),
    message: readString(bc)
  };
}
function writeRejectedResponse(bc, x) {
  writeString(bc, x.code);
  writeString(bc, x.message);
}
function readVmFetchResponse(bc) {
  return {
    responseJson: readString(bc)
  };
}
function writeVmFetchResponse(bc, x) {
  writeString(bc, x.responseJson);
}
function readResponsePayload(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "AuthenticatedResponse", val: readAuthenticatedResponse(bc) };
    case 1:
      return { tag: "SessionOpenedResponse", val: readSessionOpenedResponse(bc) };
    case 2:
      return { tag: "VmCreatedResponse", val: readVmCreatedResponse(bc) };
    case 3:
      return { tag: "VmDisposedResponse", val: readVmDisposedResponse(bc) };
    case 4:
      return { tag: "RootFilesystemBootstrappedResponse", val: readRootFilesystemBootstrappedResponse(bc) };
    case 5:
      return { tag: "VmConfiguredResponse", val: readVmConfiguredResponse(bc) };
    case 6:
      return { tag: "HostCallbacksRegisteredResponse", val: readHostCallbacksRegisteredResponse(bc) };
    case 7:
      return { tag: "LayerCreatedResponse", val: readLayerCreatedResponse(bc) };
    case 8:
      return { tag: "LayerSealedResponse", val: readLayerSealedResponse(bc) };
    case 9:
      return { tag: "SnapshotImportedResponse", val: readSnapshotImportedResponse(bc) };
    case 10:
      return { tag: "SnapshotExportedResponse", val: readSnapshotExportedResponse(bc) };
    case 11:
      return { tag: "OverlayCreatedResponse", val: readOverlayCreatedResponse(bc) };
    case 12:
      return { tag: "GuestFilesystemResultResponse", val: readGuestFilesystemResultResponse(bc) };
    case 13:
      return { tag: "RootFilesystemSnapshotResponse", val: readRootFilesystemSnapshotResponse(bc) };
    case 14:
      return { tag: "ProcessStartedResponse", val: readProcessStartedResponse(bc) };
    case 15:
      return { tag: "StdinWrittenResponse", val: readStdinWrittenResponse(bc) };
    case 16:
      return { tag: "StdinClosedResponse", val: readStdinClosedResponse(bc) };
    case 17:
      return { tag: "ProcessKilledResponse", val: readProcessKilledResponse(bc) };
    case 18:
      return { tag: "ProcessSnapshotResponse", val: readProcessSnapshotResponse(bc) };
    case 19:
      return { tag: "ListenerSnapshotResponse", val: readListenerSnapshotResponse(bc) };
    case 20:
      return { tag: "BoundUdpSnapshotResponse", val: readBoundUdpSnapshotResponse(bc) };
    case 21:
      return { tag: "SignalStateResponse", val: readSignalStateResponse(bc) };
    case 22:
      return { tag: "ZombieTimerCountResponse", val: readZombieTimerCountResponse(bc) };
    case 23:
      return { tag: "FilesystemResultResponse", val: readFilesystemResultResponse(bc) };
    case 24:
      return { tag: "PermissionDecisionResponse", val: readPermissionDecisionResponse(bc) };
    case 25:
      return { tag: "PersistenceStateResponse", val: readPersistenceStateResponse(bc) };
    case 26:
      return { tag: "PersistenceFlushedResponse", val: readPersistenceFlushedResponse(bc) };
    case 27:
      return { tag: "RejectedResponse", val: readRejectedResponse(bc) };
    case 28:
      return { tag: "VmFetchResponse", val: readVmFetchResponse(bc) };
    case 29:
      return { tag: "ExtEnvelope", val: readExtEnvelope(bc) };
    case 30:
      return { tag: "GuestKernelResultResponse", val: readGuestKernelResultResponse(bc) };
    case 31:
      return { tag: "PtyResizedResponse", val: readPtyResizedResponse(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeResponsePayload(bc, x) {
  switch (x.tag) {
    case "AuthenticatedResponse": {
      writeU8(bc, 0);
      writeAuthenticatedResponse(bc, x.val);
      break;
    }
    case "SessionOpenedResponse": {
      writeU8(bc, 1);
      writeSessionOpenedResponse(bc, x.val);
      break;
    }
    case "VmCreatedResponse": {
      writeU8(bc, 2);
      writeVmCreatedResponse(bc, x.val);
      break;
    }
    case "VmDisposedResponse": {
      writeU8(bc, 3);
      writeVmDisposedResponse(bc, x.val);
      break;
    }
    case "RootFilesystemBootstrappedResponse": {
      writeU8(bc, 4);
      writeRootFilesystemBootstrappedResponse(bc, x.val);
      break;
    }
    case "VmConfiguredResponse": {
      writeU8(bc, 5);
      writeVmConfiguredResponse(bc, x.val);
      break;
    }
    case "HostCallbacksRegisteredResponse": {
      writeU8(bc, 6);
      writeHostCallbacksRegisteredResponse(bc, x.val);
      break;
    }
    case "LayerCreatedResponse": {
      writeU8(bc, 7);
      writeLayerCreatedResponse(bc, x.val);
      break;
    }
    case "LayerSealedResponse": {
      writeU8(bc, 8);
      writeLayerSealedResponse(bc, x.val);
      break;
    }
    case "SnapshotImportedResponse": {
      writeU8(bc, 9);
      writeSnapshotImportedResponse(bc, x.val);
      break;
    }
    case "SnapshotExportedResponse": {
      writeU8(bc, 10);
      writeSnapshotExportedResponse(bc, x.val);
      break;
    }
    case "OverlayCreatedResponse": {
      writeU8(bc, 11);
      writeOverlayCreatedResponse(bc, x.val);
      break;
    }
    case "GuestFilesystemResultResponse": {
      writeU8(bc, 12);
      writeGuestFilesystemResultResponse(bc, x.val);
      break;
    }
    case "RootFilesystemSnapshotResponse": {
      writeU8(bc, 13);
      writeRootFilesystemSnapshotResponse(bc, x.val);
      break;
    }
    case "ProcessStartedResponse": {
      writeU8(bc, 14);
      writeProcessStartedResponse(bc, x.val);
      break;
    }
    case "StdinWrittenResponse": {
      writeU8(bc, 15);
      writeStdinWrittenResponse(bc, x.val);
      break;
    }
    case "StdinClosedResponse": {
      writeU8(bc, 16);
      writeStdinClosedResponse(bc, x.val);
      break;
    }
    case "ProcessKilledResponse": {
      writeU8(bc, 17);
      writeProcessKilledResponse(bc, x.val);
      break;
    }
    case "ProcessSnapshotResponse": {
      writeU8(bc, 18);
      writeProcessSnapshotResponse(bc, x.val);
      break;
    }
    case "ListenerSnapshotResponse": {
      writeU8(bc, 19);
      writeListenerSnapshotResponse(bc, x.val);
      break;
    }
    case "BoundUdpSnapshotResponse": {
      writeU8(bc, 20);
      writeBoundUdpSnapshotResponse(bc, x.val);
      break;
    }
    case "SignalStateResponse": {
      writeU8(bc, 21);
      writeSignalStateResponse(bc, x.val);
      break;
    }
    case "ZombieTimerCountResponse": {
      writeU8(bc, 22);
      writeZombieTimerCountResponse(bc, x.val);
      break;
    }
    case "FilesystemResultResponse": {
      writeU8(bc, 23);
      writeFilesystemResultResponse(bc, x.val);
      break;
    }
    case "PermissionDecisionResponse": {
      writeU8(bc, 24);
      writePermissionDecisionResponse(bc, x.val);
      break;
    }
    case "PersistenceStateResponse": {
      writeU8(bc, 25);
      writePersistenceStateResponse(bc, x.val);
      break;
    }
    case "PersistenceFlushedResponse": {
      writeU8(bc, 26);
      writePersistenceFlushedResponse(bc, x.val);
      break;
    }
    case "RejectedResponse": {
      writeU8(bc, 27);
      writeRejectedResponse(bc, x.val);
      break;
    }
    case "VmFetchResponse": {
      writeU8(bc, 28);
      writeVmFetchResponse(bc, x.val);
      break;
    }
    case "ExtEnvelope": {
      writeU8(bc, 29);
      writeExtEnvelope(bc, x.val);
      break;
    }
    case "GuestKernelResultResponse": {
      writeU8(bc, 30);
      writeGuestKernelResultResponse(bc, x.val);
      break;
    }
    case "PtyResizedResponse": {
      writeU8(bc, 31);
      writePtyResizedResponse(bc, x.val);
      break;
    }
  }
}
function readResponseFrame(bc) {
  return {
    schema: readProtocolSchema(bc),
    requestId: readRequestId(bc),
    ownership: readOwnershipScope(bc),
    payload: readResponsePayload(bc)
  };
}
function writeResponseFrame(bc, x) {
  writeProtocolSchema(bc, x.schema);
  writeRequestId(bc, x.requestId);
  writeOwnershipScope(bc, x.ownership);
  writeResponsePayload(bc, x.payload);
}
function readVmLifecycleState(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return VmLifecycleState.Creating;
    case 1:
      return VmLifecycleState.Ready;
    case 2:
      return VmLifecycleState.Disposing;
    case 3:
      return VmLifecycleState.Disposed;
    case 4:
      return VmLifecycleState.Failed;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeVmLifecycleState(bc, x) {
  switch (x) {
    case VmLifecycleState.Creating: {
      writeU8(bc, 0);
      break;
    }
    case VmLifecycleState.Ready: {
      writeU8(bc, 1);
      break;
    }
    case VmLifecycleState.Disposing: {
      writeU8(bc, 2);
      break;
    }
    case VmLifecycleState.Disposed: {
      writeU8(bc, 3);
      break;
    }
    case VmLifecycleState.Failed: {
      writeU8(bc, 4);
      break;
    }
  }
}
function readVmLifecycleEvent(bc) {
  return {
    state: readVmLifecycleState(bc)
  };
}
function writeVmLifecycleEvent(bc, x) {
  writeVmLifecycleState(bc, x.state);
}
function readStreamChannel(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return StreamChannel.Stdout;
    case 1:
      return StreamChannel.Stderr;
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeStreamChannel(bc, x) {
  switch (x) {
    case StreamChannel.Stdout: {
      writeU8(bc, 0);
      break;
    }
    case StreamChannel.Stderr: {
      writeU8(bc, 1);
      break;
    }
  }
}
function readProcessOutputEvent(bc) {
  return {
    processId: readString(bc),
    channel: readStreamChannel(bc),
    chunk: readData(bc)
  };
}
function writeProcessOutputEvent(bc, x) {
  writeString(bc, x.processId);
  writeStreamChannel(bc, x.channel);
  writeData(bc, x.chunk);
}
function readProcessExitedEvent(bc) {
  return {
    processId: readString(bc),
    exitCode: readI32(bc)
  };
}
function writeProcessExitedEvent(bc, x) {
  writeString(bc, x.processId);
  writeI32(bc, x.exitCode);
}
function readStructuredEvent(bc) {
  return {
    name: readString(bc),
    detail: read1(bc)
  };
}
function writeStructuredEvent(bc, x) {
  writeString(bc, x.name);
  write1(bc, x.detail);
}
function readEventPayload(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "VmLifecycleEvent", val: readVmLifecycleEvent(bc) };
    case 1:
      return { tag: "ProcessOutputEvent", val: readProcessOutputEvent(bc) };
    case 2:
      return { tag: "ProcessExitedEvent", val: readProcessExitedEvent(bc) };
    case 3:
      return { tag: "StructuredEvent", val: readStructuredEvent(bc) };
    case 4:
      return { tag: "ExtEnvelope", val: readExtEnvelope(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeEventPayload(bc, x) {
  switch (x.tag) {
    case "VmLifecycleEvent": {
      writeU8(bc, 0);
      writeVmLifecycleEvent(bc, x.val);
      break;
    }
    case "ProcessOutputEvent": {
      writeU8(bc, 1);
      writeProcessOutputEvent(bc, x.val);
      break;
    }
    case "ProcessExitedEvent": {
      writeU8(bc, 2);
      writeProcessExitedEvent(bc, x.val);
      break;
    }
    case "StructuredEvent": {
      writeU8(bc, 3);
      writeStructuredEvent(bc, x.val);
      break;
    }
    case "ExtEnvelope": {
      writeU8(bc, 4);
      writeExtEnvelope(bc, x.val);
      break;
    }
  }
}
function readEventFrame(bc) {
  return {
    schema: readProtocolSchema(bc),
    ownership: readOwnershipScope(bc),
    payload: readEventPayload(bc)
  };
}
function writeEventFrame(bc, x) {
  writeProtocolSchema(bc, x.schema);
  writeOwnershipScope(bc, x.ownership);
  writeEventPayload(bc, x.payload);
}
function readHostCallbackRequest(bc) {
  return {
    invocationId: readString(bc),
    callbackKey: readString(bc),
    input: readJsonUtf8(bc),
    timeoutMs: readU64(bc)
  };
}
function writeHostCallbackRequest(bc, x) {
  writeString(bc, x.invocationId);
  writeString(bc, x.callbackKey);
  writeJsonUtf8(bc, x.input);
  writeU64(bc, x.timeoutMs);
}
function readJsBridgeCallRequest(bc) {
  return {
    callId: readString(bc),
    mountId: readString(bc),
    operation: readString(bc),
    args: readJsonUtf8(bc)
  };
}
function writeJsBridgeCallRequest(bc, x) {
  writeString(bc, x.callId);
  writeString(bc, x.mountId);
  writeString(bc, x.operation);
  writeJsonUtf8(bc, x.args);
}
function readSidecarRequestPayload(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "HostCallbackRequest", val: readHostCallbackRequest(bc) };
    case 1:
      return { tag: "JsBridgeCallRequest", val: readJsBridgeCallRequest(bc) };
    case 2:
      return { tag: "ExtEnvelope", val: readExtEnvelope(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeSidecarRequestPayload(bc, x) {
  switch (x.tag) {
    case "HostCallbackRequest": {
      writeU8(bc, 0);
      writeHostCallbackRequest(bc, x.val);
      break;
    }
    case "JsBridgeCallRequest": {
      writeU8(bc, 1);
      writeJsBridgeCallRequest(bc, x.val);
      break;
    }
    case "ExtEnvelope": {
      writeU8(bc, 2);
      writeExtEnvelope(bc, x.val);
      break;
    }
  }
}
function readSidecarRequestFrame(bc) {
  return {
    schema: readProtocolSchema(bc),
    requestId: readRequestId(bc),
    ownership: readOwnershipScope(bc),
    payload: readSidecarRequestPayload(bc)
  };
}
function writeSidecarRequestFrame(bc, x) {
  writeProtocolSchema(bc, x.schema);
  writeRequestId(bc, x.requestId);
  writeOwnershipScope(bc, x.ownership);
  writeSidecarRequestPayload(bc, x.payload);
}
function read31(bc) {
  return readBool(bc) ? readJsonUtf8(bc) : null;
}
function write31(bc, x) {
  writeBool(bc, x != null);
  if (x != null) {
    writeJsonUtf8(bc, x);
  }
}
function readHostCallbackResultResponse(bc) {
  return {
    invocationId: readString(bc),
    result: read31(bc),
    error: read0(bc)
  };
}
function writeHostCallbackResultResponse(bc, x) {
  writeString(bc, x.invocationId);
  write31(bc, x.result);
  write0(bc, x.error);
}
function readJsBridgeResultResponse(bc) {
  return {
    callId: readString(bc),
    result: read31(bc),
    error: read0(bc)
  };
}
function writeJsBridgeResultResponse(bc, x) {
  writeString(bc, x.callId);
  write31(bc, x.result);
  write0(bc, x.error);
}
function readSidecarResponsePayload(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "HostCallbackResultResponse", val: readHostCallbackResultResponse(bc) };
    case 1:
      return { tag: "JsBridgeResultResponse", val: readJsBridgeResultResponse(bc) };
    case 2:
      return { tag: "ExtEnvelope", val: readExtEnvelope(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeSidecarResponsePayload(bc, x) {
  switch (x.tag) {
    case "HostCallbackResultResponse": {
      writeU8(bc, 0);
      writeHostCallbackResultResponse(bc, x.val);
      break;
    }
    case "JsBridgeResultResponse": {
      writeU8(bc, 1);
      writeJsBridgeResultResponse(bc, x.val);
      break;
    }
    case "ExtEnvelope": {
      writeU8(bc, 2);
      writeExtEnvelope(bc, x.val);
      break;
    }
  }
}
function readSidecarResponseFrame(bc) {
  return {
    schema: readProtocolSchema(bc),
    requestId: readRequestId(bc),
    ownership: readOwnershipScope(bc),
    payload: readSidecarResponsePayload(bc)
  };
}
function writeSidecarResponseFrame(bc, x) {
  writeProtocolSchema(bc, x.schema);
  writeRequestId(bc, x.requestId);
  writeOwnershipScope(bc, x.ownership);
  writeSidecarResponsePayload(bc, x.payload);
}
function readProtocolFrame(bc) {
  const offset = bc.offset;
  const tag = readU8(bc);
  switch (tag) {
    case 0:
      return { tag: "RequestFrame", val: readRequestFrame(bc) };
    case 1:
      return { tag: "ResponseFrame", val: readResponseFrame(bc) };
    case 2:
      return { tag: "EventFrame", val: readEventFrame(bc) };
    case 3:
      return { tag: "SidecarRequestFrame", val: readSidecarRequestFrame(bc) };
    case 4:
      return { tag: "SidecarResponseFrame", val: readSidecarResponseFrame(bc) };
    default: {
      bc.offset = offset;
      throw new BareError(offset, "invalid tag");
    }
  }
}
function writeProtocolFrame(bc, x) {
  switch (x.tag) {
    case "RequestFrame": {
      writeU8(bc, 0);
      writeRequestFrame(bc, x.val);
      break;
    }
    case "ResponseFrame": {
      writeU8(bc, 1);
      writeResponseFrame(bc, x.val);
      break;
    }
    case "EventFrame": {
      writeU8(bc, 2);
      writeEventFrame(bc, x.val);
      break;
    }
    case "SidecarRequestFrame": {
      writeU8(bc, 3);
      writeSidecarRequestFrame(bc, x.val);
      break;
    }
    case "SidecarResponseFrame": {
      writeU8(bc, 4);
      writeSidecarResponseFrame(bc, x.val);
      break;
    }
  }
}
function encodeProtocolFrame(x, config) {
  const fullConfig = config != null ? Config(config) : DEFAULT_CONFIG;
  const bc = new ByteCursor(new Uint8Array(fullConfig.initialBufferLength), fullConfig);
  writeProtocolFrame(bc, x);
  return new Uint8Array(bc.view.buffer, bc.view.byteOffset, bc.offset);
}
function decodeProtocolFrame(bytes) {
  const bc = new ByteCursor(bytes, DEFAULT_CONFIG);
  const result = readProtocolFrame(bc);
  if (bc.offset < bc.view.byteLength) {
    throw new BareError(bc.offset, "remaining bytes");
  }
  return result;
}
var DEFAULT_CONFIG, GuestRuntimeKind, RootFilesystemMode, RootFilesystemEntryKind, RootFilesystemEntryEncoding, PermissionMode, DisposeReason, WasmPermissionTier, GuestFilesystemOperation, FilesystemOperation, ProcessSnapshotStatus, SignalDispositionAction, VmLifecycleState, StreamChannel;
var init_generated_protocol = __esm({
  "../../../agent-os/packages/core/dist/generated-protocol.js"() {
    "use strict";
    init_dist();
    DEFAULT_CONFIG = /* @__PURE__ */ Config({});
    (function(GuestRuntimeKind2) {
      GuestRuntimeKind2["JavaScript"] = "JavaScript";
      GuestRuntimeKind2["Python"] = "Python";
      GuestRuntimeKind2["WebAssembly"] = "WebAssembly";
    })(GuestRuntimeKind || (GuestRuntimeKind = {}));
    (function(RootFilesystemMode2) {
      RootFilesystemMode2["Ephemeral"] = "Ephemeral";
      RootFilesystemMode2["ReadOnly"] = "ReadOnly";
    })(RootFilesystemMode || (RootFilesystemMode = {}));
    (function(RootFilesystemEntryKind2) {
      RootFilesystemEntryKind2["File"] = "File";
      RootFilesystemEntryKind2["Directory"] = "Directory";
      RootFilesystemEntryKind2["Symlink"] = "Symlink";
    })(RootFilesystemEntryKind || (RootFilesystemEntryKind = {}));
    (function(RootFilesystemEntryEncoding2) {
      RootFilesystemEntryEncoding2["UtF8"] = "UtF8";
      RootFilesystemEntryEncoding2["BasE64"] = "BasE64";
    })(RootFilesystemEntryEncoding || (RootFilesystemEntryEncoding = {}));
    (function(PermissionMode2) {
      PermissionMode2["Allow"] = "Allow";
      PermissionMode2["Ask"] = "Ask";
      PermissionMode2["Deny"] = "Deny";
    })(PermissionMode || (PermissionMode = {}));
    (function(DisposeReason2) {
      DisposeReason2["Requested"] = "Requested";
      DisposeReason2["ConnectionClosed"] = "ConnectionClosed";
      DisposeReason2["HostShutdown"] = "HostShutdown";
    })(DisposeReason || (DisposeReason = {}));
    (function(WasmPermissionTier2) {
      WasmPermissionTier2["Full"] = "Full";
      WasmPermissionTier2["ReadWrite"] = "ReadWrite";
      WasmPermissionTier2["ReadOnly"] = "ReadOnly";
      WasmPermissionTier2["Isolated"] = "Isolated";
    })(WasmPermissionTier || (WasmPermissionTier = {}));
    (function(GuestFilesystemOperation2) {
      GuestFilesystemOperation2["ReadFile"] = "ReadFile";
      GuestFilesystemOperation2["WriteFile"] = "WriteFile";
      GuestFilesystemOperation2["CreateDir"] = "CreateDir";
      GuestFilesystemOperation2["Mkdir"] = "Mkdir";
      GuestFilesystemOperation2["Exists"] = "Exists";
      GuestFilesystemOperation2["Stat"] = "Stat";
      GuestFilesystemOperation2["Lstat"] = "Lstat";
      GuestFilesystemOperation2["ReadDir"] = "ReadDir";
      GuestFilesystemOperation2["RemoveFile"] = "RemoveFile";
      GuestFilesystemOperation2["RemoveDir"] = "RemoveDir";
      GuestFilesystemOperation2["Rename"] = "Rename";
      GuestFilesystemOperation2["Realpath"] = "Realpath";
      GuestFilesystemOperation2["Symlink"] = "Symlink";
      GuestFilesystemOperation2["ReadLink"] = "ReadLink";
      GuestFilesystemOperation2["Link"] = "Link";
      GuestFilesystemOperation2["Chmod"] = "Chmod";
      GuestFilesystemOperation2["Chown"] = "Chown";
      GuestFilesystemOperation2["Utimes"] = "Utimes";
      GuestFilesystemOperation2["Truncate"] = "Truncate";
      GuestFilesystemOperation2["Pread"] = "Pread";
      GuestFilesystemOperation2["Pwrite"] = "Pwrite";
    })(GuestFilesystemOperation || (GuestFilesystemOperation = {}));
    (function(FilesystemOperation2) {
      FilesystemOperation2["Read"] = "Read";
      FilesystemOperation2["Write"] = "Write";
      FilesystemOperation2["Stat"] = "Stat";
      FilesystemOperation2["ReadDir"] = "ReadDir";
      FilesystemOperation2["Mkdir"] = "Mkdir";
      FilesystemOperation2["Remove"] = "Remove";
      FilesystemOperation2["Rename"] = "Rename";
    })(FilesystemOperation || (FilesystemOperation = {}));
    (function(ProcessSnapshotStatus2) {
      ProcessSnapshotStatus2["Running"] = "Running";
      ProcessSnapshotStatus2["Exited"] = "Exited";
      ProcessSnapshotStatus2["Stopped"] = "Stopped";
    })(ProcessSnapshotStatus || (ProcessSnapshotStatus = {}));
    (function(SignalDispositionAction2) {
      SignalDispositionAction2["Default"] = "Default";
      SignalDispositionAction2["Ignore"] = "Ignore";
      SignalDispositionAction2["User"] = "User";
    })(SignalDispositionAction || (SignalDispositionAction = {}));
    (function(VmLifecycleState2) {
      VmLifecycleState2["Creating"] = "Creating";
      VmLifecycleState2["Ready"] = "Ready";
      VmLifecycleState2["Disposing"] = "Disposing";
      VmLifecycleState2["Disposed"] = "Disposed";
      VmLifecycleState2["Failed"] = "Failed";
    })(VmLifecycleState || (VmLifecycleState = {}));
    (function(StreamChannel2) {
      StreamChannel2["Stdout"] = "Stdout";
      StreamChannel2["Stderr"] = "Stderr";
    })(StreamChannel || (StreamChannel = {}));
  }
});

// ../../../agent-os/packages/core/dist/protocol-maps.js
function toGeneratedPermissionMode(mode) {
  switch (mode) {
    case "allow":
      return PermissionMode.Allow;
    case "ask":
      return PermissionMode.Ask;
    case "deny":
      return PermissionMode.Deny;
  }
}
function toGeneratedGuestRuntimeKind(runtime) {
  switch (runtime) {
    case "java_script":
      return GuestRuntimeKind.JavaScript;
    case "python":
      return GuestRuntimeKind.Python;
    case "web_assembly":
      return GuestRuntimeKind.WebAssembly;
  }
}
function toGeneratedDisposeReason(reason) {
  switch (reason) {
    case "requested":
      return DisposeReason.Requested;
    case "connection_closed":
      return DisposeReason.ConnectionClosed;
    case "host_shutdown":
      return DisposeReason.HostShutdown;
  }
}
function toGeneratedRootFilesystemMode(mode) {
  switch (mode) {
    case "ephemeral":
      return RootFilesystemMode.Ephemeral;
    case "read_only":
      return RootFilesystemMode.ReadOnly;
  }
}
function toGeneratedRootFilesystemEntryKind(kind) {
  switch (kind) {
    case "file":
      return RootFilesystemEntryKind.File;
    case "directory":
      return RootFilesystemEntryKind.Directory;
    case "symlink":
      return RootFilesystemEntryKind.Symlink;
  }
}
function toGeneratedRootFilesystemEntryEncoding(encoding) {
  switch (encoding) {
    case "utf8":
      return RootFilesystemEntryEncoding.UtF8;
    case "base64":
      return RootFilesystemEntryEncoding.BasE64;
  }
}
function toGeneratedWasmPermissionTier(tier) {
  switch (tier) {
    case "full":
      return WasmPermissionTier.Full;
    case "read-write":
      return WasmPermissionTier.ReadWrite;
    case "read-only":
      return WasmPermissionTier.ReadOnly;
    case "isolated":
      return WasmPermissionTier.Isolated;
  }
}
function toGeneratedGuestFilesystemOperation(operation) {
  switch (operation) {
    case "read_file":
      return GuestFilesystemOperation.ReadFile;
    case "write_file":
      return GuestFilesystemOperation.WriteFile;
    case "create_dir":
      return GuestFilesystemOperation.CreateDir;
    case "mkdir":
      return GuestFilesystemOperation.Mkdir;
    case "exists":
      return GuestFilesystemOperation.Exists;
    case "stat":
      return GuestFilesystemOperation.Stat;
    case "lstat":
      return GuestFilesystemOperation.Lstat;
    case "read_dir":
      return GuestFilesystemOperation.ReadDir;
    case "remove_file":
      return GuestFilesystemOperation.RemoveFile;
    case "remove_dir":
      return GuestFilesystemOperation.RemoveDir;
    case "rename":
      return GuestFilesystemOperation.Rename;
    case "realpath":
      return GuestFilesystemOperation.Realpath;
    case "symlink":
      return GuestFilesystemOperation.Symlink;
    case "read_link":
      return GuestFilesystemOperation.ReadLink;
    case "link":
      return GuestFilesystemOperation.Link;
    case "chmod":
      return GuestFilesystemOperation.Chmod;
    case "chown":
      return GuestFilesystemOperation.Chown;
    case "utimes":
      return GuestFilesystemOperation.Utimes;
    case "truncate":
      return GuestFilesystemOperation.Truncate;
    case "pread":
      return GuestFilesystemOperation.Pread;
    case "pwrite":
      return GuestFilesystemOperation.Pwrite;
  }
}
function toGeneratedFilesystemOperation(operation) {
  switch (operation) {
    case "read":
      return FilesystemOperation.Read;
    case "write":
      return FilesystemOperation.Write;
    case "stat":
      return FilesystemOperation.Stat;
    case "read_dir":
      return FilesystemOperation.ReadDir;
    case "mkdir":
      return FilesystemOperation.Mkdir;
    case "remove":
      return FilesystemOperation.Remove;
    case "rename":
      return FilesystemOperation.Rename;
  }
}
function fromGeneratedFilesystemOperation(operation) {
  switch (operation) {
    case FilesystemOperation.Read:
      return "read";
    case FilesystemOperation.Write:
      return "write";
    case FilesystemOperation.Stat:
      return "stat";
    case FilesystemOperation.ReadDir:
      return "read_dir";
    case FilesystemOperation.Mkdir:
      return "mkdir";
    case FilesystemOperation.Remove:
      return "remove";
    case FilesystemOperation.Rename:
      return "rename";
  }
}
function fromGeneratedVmLifecycleState(state) {
  switch (state) {
    case VmLifecycleState.Creating:
      return "creating";
    case VmLifecycleState.Ready:
      return "ready";
    case VmLifecycleState.Disposing:
      return "disposing";
    case VmLifecycleState.Disposed:
      return "disposed";
    case VmLifecycleState.Failed:
      return "failed";
  }
}
function fromGeneratedStreamChannel(channel) {
  switch (channel) {
    case StreamChannel.Stdout:
      return "stdout";
    case StreamChannel.Stderr:
      return "stderr";
  }
}
function fromGeneratedProcessSnapshotStatus(status2) {
  switch (status2) {
    case ProcessSnapshotStatus.Running:
      return "running";
    case ProcessSnapshotStatus.Exited:
      return "exited";
    case ProcessSnapshotStatus.Stopped:
      return "stopped";
  }
}
function fromGeneratedSignalDispositionAction(action) {
  switch (action) {
    case SignalDispositionAction.Default:
      return "default";
    case SignalDispositionAction.Ignore:
      return "ignore";
    case SignalDispositionAction.User:
      return "user";
  }
}
function fromGeneratedRootFilesystemEntryKind(kind) {
  switch (kind) {
    case RootFilesystemEntryKind.File:
      return "file";
    case RootFilesystemEntryKind.Directory:
      return "directory";
    case RootFilesystemEntryKind.Symlink:
      return "symlink";
  }
}
function fromGeneratedRootFilesystemEntryEncoding(encoding) {
  switch (encoding) {
    case RootFilesystemEntryEncoding.UtF8:
      return "utf8";
    case RootFilesystemEntryEncoding.BasE64:
      return "base64";
  }
}
function fromGeneratedGuestFilesystemOperation(operation) {
  switch (operation) {
    case GuestFilesystemOperation.ReadFile:
      return "read_file";
    case GuestFilesystemOperation.WriteFile:
      return "write_file";
    case GuestFilesystemOperation.CreateDir:
      return "create_dir";
    case GuestFilesystemOperation.Mkdir:
      return "mkdir";
    case GuestFilesystemOperation.Exists:
      return "exists";
    case GuestFilesystemOperation.Stat:
      return "stat";
    case GuestFilesystemOperation.Lstat:
      return "lstat";
    case GuestFilesystemOperation.ReadDir:
      return "read_dir";
    case GuestFilesystemOperation.RemoveFile:
      return "remove_file";
    case GuestFilesystemOperation.RemoveDir:
      return "remove_dir";
    case GuestFilesystemOperation.Rename:
      return "rename";
    case GuestFilesystemOperation.Realpath:
      return "realpath";
    case GuestFilesystemOperation.Symlink:
      return "symlink";
    case GuestFilesystemOperation.ReadLink:
      return "read_link";
    case GuestFilesystemOperation.Link:
      return "link";
    case GuestFilesystemOperation.Chmod:
      return "chmod";
    case GuestFilesystemOperation.Chown:
      return "chown";
    case GuestFilesystemOperation.Utimes:
      return "utimes";
    case GuestFilesystemOperation.Truncate:
      return "truncate";
    case GuestFilesystemOperation.Pread:
      return "pread";
    case GuestFilesystemOperation.Pwrite:
      return "pwrite";
  }
}
var init_protocol_maps = __esm({
  "../../../agent-os/packages/core/dist/protocol-maps.js"() {
    "use strict";
    init_generated_protocol();
  }
});

// ../../../agent-os/packages/core/dist/event-buffer.js
function fromGeneratedEventPayload(payload) {
  switch (payload.tag) {
    case "VmLifecycleEvent":
      return {
        type: "vm_lifecycle",
        state: fromGeneratedVmLifecycleState(payload.val.state)
      };
    case "ProcessOutputEvent":
      return {
        type: "process_output",
        process_id: payload.val.processId,
        channel: fromGeneratedStreamChannel(payload.val.channel),
        chunk: Buffer.from(payload.val.chunk)
      };
    case "ProcessExitedEvent":
      return {
        type: "process_exited",
        process_id: payload.val.processId,
        exit_code: payload.val.exitCode
      };
    case "StructuredEvent":
      return {
        type: "structured",
        name: payload.val.name,
        detail: Object.fromEntries(payload.val.detail)
      };
    case "ExtEnvelope":
      return {
        type: "ext",
        envelope: fromGeneratedExtEnvelope(payload.val)
      };
  }
}
var init_event_buffer = __esm({
  "../../../agent-os/packages/core/dist/event-buffer.js"() {
    "use strict";
    init_ext();
    init_ownership();
    init_protocol_maps();
  }
});

// ../../../agent-os/packages/core/dist/protocol-schema.js
function validateSidecarProtocolSchema(schema) {
  if (schema.name !== SIDECAR_PROTOCOL_SCHEMA.name || schema.version !== SIDECAR_PROTOCOL_SCHEMA.version) {
    throw new Error(`unsupported sidecar protocol schema ${schema.name}@${schema.version}`);
  }
  return SIDECAR_PROTOCOL_SCHEMA;
}
var SIDECAR_PROTOCOL_SCHEMA;
var init_protocol_schema = __esm({
  "../../../agent-os/packages/core/dist/protocol-schema.js"() {
    "use strict";
    SIDECAR_PROTOCOL_SCHEMA = {
      name: "agentos-native-sidecar",
      version: 7
    };
  }
});

// ../../../agent-os/packages/core/dist/descriptors.js
function toGeneratedSidecarPlacement(placement) {
  switch (placement.kind) {
    case "shared":
      return {
        tag: "SidecarPlacementShared",
        val: { pool: placement.pool ?? null }
      };
    case "explicit":
      return {
        tag: "SidecarPlacementExplicit",
        val: { sidecarId: placement.sidecar_id }
      };
  }
}
function toGeneratedMountDescriptor(descriptor) {
  return {
    guestPath: descriptor.guest_path,
    readOnly: descriptor.read_only,
    plugin: {
      id: descriptor.plugin.id,
      config: stringifyJsonUtf8(descriptor.plugin.config ?? {}, "mount plugin config")
    }
  };
}
function toGeneratedSoftwareDescriptor(descriptor) {
  return {
    packageName: descriptor.package_name,
    root: descriptor.root
  };
}
function toGeneratedProjectedModuleDescriptor(descriptor) {
  return {
    packageName: descriptor.package_name,
    entrypoint: descriptor.entrypoint
  };
}
var init_descriptors = __esm({
  "../../../agent-os/packages/core/dist/descriptors.js"() {
    "use strict";
    init_json();
  }
});

// ../../../agent-os/packages/core/dist/filesystem.js
function toGeneratedRootFilesystemEntry(entry) {
  return {
    path: entry.path,
    kind: toGeneratedRootFilesystemEntryKind(entry.kind),
    mode: entry.mode ?? null,
    uid: entry.uid ?? null,
    gid: entry.gid ?? null,
    content: entry.content ?? null,
    encoding: entry.encoding === void 0 ? null : toGeneratedRootFilesystemEntryEncoding(entry.encoding),
    target: entry.target ?? null,
    executable: entry.executable ?? false
  };
}
function fromGeneratedRootFilesystemEntry(entry) {
  return {
    path: entry.path,
    kind: fromGeneratedRootFilesystemEntryKind(entry.kind),
    ...entry.mode !== null ? { mode: entry.mode } : {},
    ...entry.uid !== null ? { uid: entry.uid } : {},
    ...entry.gid !== null ? { gid: entry.gid } : {},
    ...entry.content !== null ? { content: entry.content } : {},
    ...entry.encoding !== null ? { encoding: fromGeneratedRootFilesystemEntryEncoding(entry.encoding) } : {},
    ...entry.target !== null ? { target: entry.target } : {},
    executable: entry.executable
  };
}
var init_filesystem = __esm({
  "../../../agent-os/packages/core/dist/filesystem.js"() {
    "use strict";
    init_protocol_maps();
  }
});

// ../../../agent-os/packages/core/dist/permissions.js
function toGeneratedPermissionsPolicy(policy) {
  if (policy === void 0) {
    return null;
  }
  return {
    fs: policy.fs === void 0 ? null : toGeneratedFilesystemPermissionScope(policy.fs),
    network: policy.network === void 0 ? null : toGeneratedPatternPermissionScope(policy.network),
    childProcess: policy.child_process === void 0 ? null : toGeneratedPatternPermissionScope(policy.child_process),
    process: policy.process === void 0 ? null : toGeneratedPatternPermissionScope(policy.process),
    env: policy.env === void 0 ? null : toGeneratedPatternPermissionScope(policy.env),
    binding: policy.binding === void 0 ? null : toGeneratedPatternPermissionScope(policy.binding)
  };
}
function toGeneratedFilesystemPermissionScope(scope) {
  if (typeof scope === "string") {
    return {
      tag: "PermissionMode",
      val: toGeneratedPermissionMode(scope)
    };
  }
  return {
    tag: "FsPermissionRuleSet",
    val: {
      default: scope.default === void 0 ? null : toGeneratedPermissionMode(scope.default),
      rules: scope.rules.map((rule) => ({
        mode: toGeneratedPermissionMode(rule.mode),
        operations: rule.operations ?? [],
        paths: rule.paths ?? []
      }))
    }
  };
}
function toGeneratedPatternPermissionScope(scope) {
  if (typeof scope === "string") {
    return {
      tag: "PermissionMode",
      val: toGeneratedPermissionMode(scope)
    };
  }
  return {
    tag: "PatternPermissionRuleSet",
    val: {
      default: scope.default === void 0 ? null : toGeneratedPermissionMode(scope.default),
      rules: scope.rules.map((rule) => ({
        mode: toGeneratedPermissionMode(rule.mode),
        operations: rule.operations ?? [],
        patterns: rule.patterns ?? []
      }))
    }
  };
}
var init_permissions = __esm({
  "../../../agent-os/packages/core/dist/permissions.js"() {
    "use strict";
    init_protocol_maps();
  }
});

// ../../../agent-os/packages/core/dist/request-payloads.js
function toGeneratedRequestPayload(payload) {
  switch (payload.type) {
    case "authenticate":
      return {
        tag: "AuthenticateRequest",
        val: {
          clientName: payload.client_name,
          authToken: payload.auth_token,
          protocolVersion: payload.protocol_version,
          bridgeVersion: payload.bridge_version
        }
      };
    case "open_session":
      return {
        tag: "OpenSessionRequest",
        val: {
          placement: toGeneratedSidecarPlacement(payload.placement),
          metadata: new Map(Object.entries(payload.metadata ?? {}))
        }
      };
    case "create_vm":
      return {
        tag: "CreateVmRequest",
        val: {
          runtime: toGeneratedGuestRuntimeKind(payload.runtime),
          config: stringifyJsonUtf8(payload.config, "create VM config")
        }
      };
    case "dispose_vm":
      return {
        tag: "DisposeVmRequest",
        val: { reason: toGeneratedDisposeReason(payload.reason) }
      };
    case "bootstrap_root_filesystem":
      return {
        tag: "BootstrapRootFilesystemRequest",
        val: { entries: payload.entries.map(toGeneratedRootFilesystemEntry) }
      };
    case "configure_vm":
      return {
        tag: "ConfigureVmRequest",
        val: {
          mounts: (payload.mounts ?? []).map(toGeneratedMountDescriptor),
          software: (payload.software ?? []).map(toGeneratedSoftwareDescriptor),
          permissions: toGeneratedPermissionsPolicy(payload.permissions),
          moduleAccessCwd: payload.module_access_cwd ?? null,
          instructions: payload.instructions ?? [],
          projectedModules: (payload.projected_modules ?? []).map(toGeneratedProjectedModuleDescriptor),
          commandPermissions: new Map(Object.entries(payload.command_permissions ?? {}).map(([name, tier]) => [name, toGeneratedWasmPermissionTier(tier)])),
          loopbackExemptPorts: new Uint16Array(payload.loopback_exempt_ports ?? [])
        }
      };
    case "register_host_callbacks":
      return {
        tag: "RegisterHostCallbacksRequest",
        val: {
          name: payload.name,
          description: payload.description,
          commandAliases: payload.command_aliases ?? [],
          registryCommandAliases: payload.registry_command_aliases ?? [],
          callbacks: new Map(Object.entries(payload.callbacks).map(([name, callback]) => [
            name,
            {
              description: callback.description,
              inputSchema: stringifyJsonUtf8(callback.input_schema, "register_host_callbacks.callback.input_schema"),
              timeoutMs: callback.timeout_ms === void 0 ? null : BigInt(callback.timeout_ms),
              examples: (callback.examples ?? []).map((example) => ({
                description: example.description,
                input: stringifyJsonUtf8(example.input, "register_host_callbacks.callback.example.input")
              }))
            }
          ]))
        }
      };
    case "create_layer":
      return { tag: "CreateLayerRequest", val: null };
    case "seal_layer":
      return { tag: "SealLayerRequest", val: { layerId: payload.layer_id } };
    case "import_snapshot":
      return {
        tag: "ImportSnapshotRequest",
        val: { entries: payload.entries.map(toGeneratedRootFilesystemEntry) }
      };
    case "export_snapshot":
      return {
        tag: "ExportSnapshotRequest",
        val: { layerId: payload.layer_id }
      };
    case "create_overlay":
      return {
        tag: "CreateOverlayRequest",
        val: {
          mode: toGeneratedRootFilesystemMode(payload.mode ?? "ephemeral"),
          upperLayerId: payload.upper_layer_id ?? null,
          lowerLayerIds: payload.lower_layer_ids ?? []
        }
      };
    case "guest_filesystem_call":
      return {
        tag: "GuestFilesystemCallRequest",
        val: {
          operation: toGeneratedGuestFilesystemOperation(payload.operation),
          path: payload.path,
          destinationPath: payload.destination_path ?? null,
          target: payload.target ?? null,
          content: payload.content ?? null,
          encoding: payload.encoding === void 0 ? null : toGeneratedRootFilesystemEntryEncoding(payload.encoding),
          recursive: payload.recursive ?? false,
          mode: payload.mode ?? null,
          uid: payload.uid ?? null,
          gid: payload.gid ?? null,
          atimeMs: toGeneratedOptionalU64(payload.atime_ms),
          mtimeMs: toGeneratedOptionalU64(payload.mtime_ms),
          len: toGeneratedOptionalU64(payload.len),
          offset: toGeneratedOptionalU64(payload.offset)
        }
      };
    case "guest_kernel_call":
      return {
        tag: "GuestKernelCallRequest",
        val: {
          executionId: payload.execution_id,
          operation: payload.operation,
          payload: payload.payload
        }
      };
    case "snapshot_root_filesystem":
      return { tag: "SnapshotRootFilesystemRequest", val: null };
    case "execute":
      return {
        tag: "ExecuteRequest",
        val: {
          processId: payload.process_id,
          command: payload.command ?? null,
          runtime: payload.runtime === void 0 ? null : toGeneratedGuestRuntimeKind(payload.runtime),
          entrypoint: payload.entrypoint ?? null,
          args: payload.args ?? [],
          env: new Map(Object.entries(payload.env ?? {})),
          cwd: payload.cwd ?? null,
          wasmPermissionTier: payload.wasm_permission_tier === void 0 ? null : toGeneratedWasmPermissionTier(payload.wasm_permission_tier)
        }
      };
    case "write_stdin":
      return {
        tag: "WriteStdinRequest",
        val: {
          processId: payload.process_id,
          chunk: toExactArrayBuffer(payload.chunk)
        }
      };
    case "resize_pty":
      return {
        tag: "ResizePtyRequest",
        val: {
          processId: payload.process_id,
          cols: payload.cols,
          rows: payload.rows
        }
      };
    case "close_stdin":
      return {
        tag: "CloseStdinRequest",
        val: { processId: payload.process_id }
      };
    case "kill_process":
      return {
        tag: "KillProcessRequest",
        val: { processId: payload.process_id, signal: payload.signal }
      };
    case "get_process_snapshot":
      return { tag: "GetProcessSnapshotRequest", val: null };
    case "find_listener":
      return {
        tag: "FindListenerRequest",
        val: {
          host: payload.host ?? null,
          port: payload.port ?? null,
          path: payload.path ?? null
        }
      };
    case "find_bound_udp":
      return {
        tag: "FindBoundUdpRequest",
        val: { host: payload.host ?? null, port: payload.port ?? null }
      };
    case "vm_fetch":
      return {
        tag: "VmFetchRequest",
        val: {
          port: payload.port,
          method: payload.method,
          path: payload.path,
          headersJson: payload.headers_json,
          body: payload.body ?? null
        }
      };
    case "get_signal_state":
      return {
        tag: "GetSignalStateRequest",
        val: { processId: payload.process_id }
      };
    case "get_zombie_timer_count":
      return { tag: "GetZombieTimerCountRequest", val: null };
    case "host_filesystem_call":
      return {
        tag: "HostFilesystemCallRequest",
        val: {
          operation: toGeneratedFilesystemOperation(payload.operation),
          path: payload.path,
          payloadSizeBytes: BigInt(payload.payload_size_bytes)
        }
      };
    case "persistence_load":
      return {
        tag: "PersistenceLoadRequest",
        val: { key: payload.key }
      };
    case "persistence_flush":
      return {
        tag: "PersistenceFlushRequest",
        val: {
          key: payload.key,
          payloadSizeBytes: BigInt(payload.payload_size_bytes)
        }
      };
    case "ext":
      return {
        tag: "ExtEnvelope",
        val: toGeneratedExtEnvelope(payload.envelope)
      };
  }
}
function toGeneratedOptionalU64(value) {
  return value === void 0 ? null : BigInt(value);
}
var init_request_payloads = __esm({
  "../../../agent-os/packages/core/dist/request-payloads.js"() {
    "use strict";
    init_bytes();
    init_descriptors();
    init_ext();
    init_filesystem();
    init_json();
    init_permissions();
    init_protocol_maps();
  }
});

// ../../../agent-os/packages/core/dist/state.js
function fromGeneratedGuestFilesystemStat(stat) {
  return {
    mode: stat.mode,
    size: bigIntToSafeNumber(stat.size, "guest filesystem stat size"),
    blocks: bigIntToSafeNumber(stat.blocks, "guest filesystem stat blocks"),
    dev: bigIntToSafeNumber(stat.dev, "guest filesystem stat dev"),
    rdev: bigIntToSafeNumber(stat.rdev, "guest filesystem stat rdev"),
    is_directory: stat.isDirectory,
    is_symbolic_link: stat.isSymbolicLink,
    atime_ms: bigIntToSafeNumber(stat.atimeMs, "guest filesystem stat atime"),
    mtime_ms: bigIntToSafeNumber(stat.mtimeMs, "guest filesystem stat mtime"),
    ctime_ms: bigIntToSafeNumber(stat.ctimeMs, "guest filesystem stat ctime"),
    birthtime_ms: bigIntToSafeNumber(stat.birthtimeMs, "guest filesystem stat birthtime"),
    ino: bigIntToSafeNumber(stat.ino, "guest filesystem stat ino"),
    nlink: bigIntToSafeNumber(stat.nlink, "guest filesystem stat nlink"),
    uid: stat.uid,
    gid: stat.gid
  };
}
function fromGeneratedSocketStateEntry(entry) {
  return {
    process_id: entry.processId,
    ...entry.host !== null ? { host: entry.host } : {},
    ...entry.port !== null ? { port: entry.port } : {},
    ...entry.path !== null ? { path: entry.path } : {}
  };
}
function fromGeneratedProcessSnapshotEntry(entry) {
  return {
    process_id: entry.processId,
    pid: entry.pid,
    ppid: entry.ppid,
    pgid: entry.pgid,
    sid: entry.sid,
    driver: entry.driver,
    command: entry.command,
    args: [...entry.args],
    cwd: entry.cwd,
    status: fromGeneratedProcessSnapshotStatus(entry.status),
    ...entry.exitCode !== null ? { exit_code: entry.exitCode } : {}
  };
}
var init_state = __esm({
  "../../../agent-os/packages/core/dist/state.js"() {
    "use strict";
    init_numbers();
    init_protocol_maps();
  }
});

// ../../../agent-os/packages/core/dist/response-payloads.js
function fromGeneratedResponsePayload(payload) {
  switch (payload.tag) {
    case "AuthenticatedResponse":
      return {
        type: "authenticated",
        sidecar_id: payload.val.sidecarId,
        connection_id: payload.val.connectionId,
        max_frame_bytes: payload.val.maxFrameBytes
      };
    case "SessionOpenedResponse":
      return {
        type: "session_opened",
        session_id: payload.val.sessionId,
        owner_connection_id: payload.val.ownerConnectionId
      };
    case "VmCreatedResponse":
      return { type: "vm_created", vm_id: payload.val.vmId };
    case "VmDisposedResponse":
      return { type: "vm_disposed", vm_id: payload.val.vmId };
    case "RootFilesystemBootstrappedResponse":
      return {
        type: "root_filesystem_bootstrapped",
        entry_count: payload.val.entryCount
      };
    case "VmConfiguredResponse":
      return {
        type: "vm_configured",
        applied_mounts: payload.val.appliedMounts,
        applied_software: payload.val.appliedSoftware
      };
    case "HostCallbacksRegisteredResponse":
      return {
        type: "host_callbacks_registered",
        registration: payload.val.registration,
        command_count: payload.val.commandCount
      };
    case "LayerCreatedResponse":
      return { type: "layer_created", layer_id: payload.val.layerId };
    case "LayerSealedResponse":
      return { type: "layer_sealed", layer_id: payload.val.layerId };
    case "SnapshotImportedResponse":
      return { type: "snapshot_imported", layer_id: payload.val.layerId };
    case "SnapshotExportedResponse":
      return {
        type: "snapshot_exported",
        layer_id: payload.val.layerId,
        entries: payload.val.entries.map(fromGeneratedRootFilesystemEntry)
      };
    case "OverlayCreatedResponse":
      return { type: "overlay_created", layer_id: payload.val.layerId };
    case "GuestFilesystemResultResponse":
      return {
        type: "guest_filesystem_result",
        operation: fromGeneratedGuestFilesystemOperation(payload.val.operation),
        path: payload.val.path,
        ...payload.val.content !== null ? { content: payload.val.content } : {},
        ...payload.val.encoding !== null ? {
          encoding: fromGeneratedRootFilesystemEntryEncoding(payload.val.encoding)
        } : {},
        ...payload.val.entries !== null ? {
          entries: payload.val.entries.map((entry) => ({
            name: entry.name,
            isDirectory: entry.isDirectory,
            isSymbolicLink: entry.isSymbolicLink
          }))
        } : {},
        ...payload.val.stat !== null ? { stat: fromGeneratedGuestFilesystemStat(payload.val.stat) } : {},
        ...payload.val.exists !== null ? { exists: payload.val.exists } : {},
        ...payload.val.target !== null ? { target: payload.val.target } : {}
      };
    case "GuestKernelResultResponse":
      return {
        type: "guest_kernel_result",
        payload: payload.val.payload
      };
    case "RootFilesystemSnapshotResponse":
      return {
        type: "root_filesystem_snapshot",
        entries: payload.val.entries.map(fromGeneratedRootFilesystemEntry)
      };
    case "ProcessStartedResponse":
      return {
        type: "process_started",
        process_id: payload.val.processId,
        ...payload.val.pid !== null ? { pid: payload.val.pid } : {}
      };
    case "StdinWrittenResponse":
      return {
        type: "stdin_written",
        process_id: payload.val.processId,
        accepted_bytes: bigIntToSafeNumber(payload.val.acceptedBytes, "stdin_written.accepted_bytes")
      };
    case "PtyResizedResponse":
      return {
        type: "pty_resized",
        process_id: payload.val.processId,
        cols: payload.val.cols,
        rows: payload.val.rows
      };
    case "StdinClosedResponse":
      return { type: "stdin_closed", process_id: payload.val.processId };
    case "ProcessKilledResponse":
      return { type: "process_killed", process_id: payload.val.processId };
    case "ProcessSnapshotResponse":
      return {
        type: "process_snapshot",
        processes: payload.val.processes.map(fromGeneratedProcessSnapshotEntry)
      };
    case "ListenerSnapshotResponse":
      return {
        type: "listener_snapshot",
        ...payload.val.listener !== null ? { listener: fromGeneratedSocketStateEntry(payload.val.listener) } : {}
      };
    case "BoundUdpSnapshotResponse":
      return {
        type: "bound_udp_snapshot",
        ...payload.val.socket !== null ? { socket: fromGeneratedSocketStateEntry(payload.val.socket) } : {}
      };
    case "SignalStateResponse":
      return {
        type: "signal_state",
        process_id: payload.val.processId,
        handlers: Object.fromEntries([...payload.val.handlers].map(([signal, registration]) => [
          String(signal),
          {
            action: fromGeneratedSignalDispositionAction(registration.action),
            mask: Array.from(registration.mask),
            flags: registration.flags
          }
        ]))
      };
    case "ZombieTimerCountResponse":
      return {
        type: "zombie_timer_count",
        count: bigIntToSafeNumber(payload.val.count, "zombie_timer_count.count")
      };
    case "FilesystemResultResponse":
      return {
        type: "filesystem_result",
        operation: fromGeneratedFilesystemOperation(payload.val.operation),
        status: payload.val.status,
        payload_size_bytes: bigIntToSafeNumber(payload.val.payloadSizeBytes, "filesystem_result.payload_size_bytes")
      };
    case "PermissionDecisionResponse":
      throw new Error("unsupported bare response payload tag: permission_decision");
    case "PersistenceStateResponse":
      return {
        type: "persistence_state",
        key: payload.val.key,
        found: payload.val.found,
        payload_size_bytes: bigIntToSafeNumber(payload.val.payloadSizeBytes, "persistence_state.payload_size_bytes")
      };
    case "PersistenceFlushedResponse":
      return {
        type: "persistence_flushed",
        key: payload.val.key,
        committed_bytes: bigIntToSafeNumber(payload.val.committedBytes, "persistence_flushed.committed_bytes")
      };
    case "RejectedResponse":
      return {
        type: "rejected",
        code: payload.val.code,
        message: payload.val.message
      };
    case "VmFetchResponse":
      return {
        type: "vm_fetch_result",
        response_json: payload.val.responseJson
      };
    case "ExtEnvelope":
      return {
        type: "ext_result",
        envelope: fromGeneratedExtEnvelope(payload.val)
      };
  }
}
var init_response_payloads = __esm({
  "../../../agent-os/packages/core/dist/response-payloads.js"() {
    "use strict";
    init_filesystem();
    init_ext();
    init_numbers();
    init_protocol_maps();
    init_state();
  }
});

// ../../../agent-os/packages/core/dist/protocol-frames.js
function toGeneratedProtocolFrame(frame) {
  switch (frame.frame_type) {
    case "request":
      return {
        tag: "RequestFrame",
        val: {
          schema: frame.schema,
          requestId: BigInt(frame.request_id),
          ownership: toGeneratedOwnershipScope(frame.ownership),
          payload: toGeneratedRequestPayload(frame.payload)
        }
      };
    case "sidecar_response":
      return {
        tag: "SidecarResponseFrame",
        val: {
          schema: frame.schema,
          requestId: BigInt(frame.request_id),
          ownership: toGeneratedOwnershipScope(frame.ownership),
          payload: toGeneratedSidecarResponsePayload(frame.payload)
        }
      };
    case "response":
    case "event":
    case "sidecar_request":
      throw new Error(`BARE encoding is only implemented for host-written frames, received ${frame.frame_type}`);
  }
}
function encodeBareProtocolFrame(frame) {
  return encodeProtocolFrame(toGeneratedProtocolFrame(frame));
}
function decodeBareProtocolFrame(payload) {
  return fromGeneratedSidecarWrittenProtocolFrame(decodeProtocolFrame(toExactUint8Array(payload)));
}
function fromGeneratedSidecarWrittenProtocolFrame(frame) {
  switch (frame.tag) {
    case "ResponseFrame":
      return {
        frame_type: "response",
        schema: toLiveProtocolSchema(frame.val.schema),
        request_id: bigIntToSafeNumber(frame.val.requestId, "response request id"),
        ownership: fromGeneratedOwnershipScope(frame.val.ownership),
        payload: fromGeneratedResponsePayload(frame.val.payload)
      };
    case "EventFrame":
      return {
        frame_type: "event",
        schema: toLiveProtocolSchema(frame.val.schema),
        ownership: fromGeneratedOwnershipScope(frame.val.ownership),
        payload: fromGeneratedEventPayload(frame.val.payload)
      };
    case "SidecarRequestFrame":
      return {
        frame_type: "sidecar_request",
        schema: toLiveProtocolSchema(frame.val.schema),
        request_id: bigIntToSafeNumber(frame.val.requestId, "sidecar request id"),
        ownership: fromGeneratedOwnershipScope(frame.val.ownership),
        payload: fromGeneratedSidecarRequestPayload(frame.val.payload)
      };
    case "RequestFrame":
    case "SidecarResponseFrame":
      throw new Error(`unsupported BARE protocol frame tag: ${frame.tag}`);
  }
}
function toLiveProtocolSchema(schema) {
  return validateSidecarProtocolSchema(schema);
}
var init_protocol_frames = __esm({
  "../../../agent-os/packages/core/dist/protocol-frames.js"() {
    "use strict";
    init_bytes();
    init_frame_payload_codec();
    init_callbacks();
    init_event_buffer();
    init_generated_protocol();
    init_numbers();
    init_ownership();
    init_protocol_schema();
    init_request_payloads();
    init_response_payloads();
  }
});

// ../../../agent-os/packages/browser/dist/encoding.js
var init_encoding = __esm({
  "../../../agent-os/packages/browser/dist/encoding.js"() {
    "use strict";
  }
});

// ../../../agent-os/packages/browser/dist/os-filesystem.js
var init_os_filesystem = __esm({
  "../../../agent-os/packages/browser/dist/os-filesystem.js"() {
    "use strict";
  }
});

// ../../../agent-os/packages/browser/dist/wasi-polyfill.js
var BROWSER_WASI_POLYFILL_CODE;
var init_wasi_polyfill = __esm({
  "../../../agent-os/packages/browser/dist/wasi-polyfill.js"() {
    "use strict";
    BROWSER_WASI_POLYFILL_CODE = `
		globalThis.__agentOSWasiHost = {
			requireBuiltin: (name) =>
				globalThis.require(String(name).replace(/^node:/, "")),
			syncReadLimitBytes: 16777216,
			// Browser fs descriptors are a JS handle table, not real host OS fds with
			// a kernel offset, so locally-opened files must use the offset-aware file
			// branches (explicit position) rather than host-passthrough null reads.
			disableLocalFdPassthrough: true,
			// Guest stdin is delivered through the runtime process object, not a kernel
			// fd, so read the queued bytes from process.stdin directly.
			readStdin: (maxBytes) =>
				(globalThis.process &&
					globalThis.process.stdin &&
					typeof globalThis.process.stdin.read === "function"
					? globalThis.process.stdin.read(maxBytes)
					: null),
			// Queued stdin byte count for poll_oneoff readiness (does not consume).
			stdinReadableBytes: () =>
				(globalThis.process && globalThis.process.stdin
					? Number(globalThis.process.stdin.readableLength || 0)
					: 0),
		};
		const Buffer =
			(typeof globalThis !== "undefined" && globalThis.Buffer) ||
			(class __AgentOsWasiBuffer extends Uint8Array {
				static alloc(size) { return new __AgentOsWasiBuffer(size >>> 0); }
				static allocUnsafe(size) { return new __AgentOsWasiBuffer(size >>> 0); }
				static isBuffer(value) { return value instanceof Uint8Array; }
				static byteLength(value, encoding) {
					if (value instanceof Uint8Array) return value.length;
					if (encoding === "base64") return Math.floor((String(value).replace(/=+$/, "").length * 3) / 4);
					if (encoding === "hex") return String(value).length >> 1;
					return new TextEncoder().encode(String(value)).length;
				}
				static from(value, encodingOrOffset, length) {
					if (typeof value === "string") {
						const encoding = encodingOrOffset || "utf8";
						if (encoding === "base64") {
							const binary = atob(value);
							const out = new __AgentOsWasiBuffer(binary.length);
							for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i) & 0xff;
							return out;
						}
						if (encoding === "hex") {
							const clean = String(value);
							const out = new __AgentOsWasiBuffer(clean.length >> 1);
							for (let i = 0; i < out.length; i += 1) out[i] = parseInt(clean.substr(i * 2, 2), 16);
							return out;
						}
						const encoded = new TextEncoder().encode(value);
						const out = new __AgentOsWasiBuffer(encoded.length);
						out.set(encoded);
						return out;
					}
					if (value instanceof ArrayBuffer) {
						const offset = encodingOrOffset || 0;
						const len = length === undefined ? value.byteLength - offset : length;
						const view = new Uint8Array(value, offset, len);
						const out = new __AgentOsWasiBuffer(view.length);
						out.set(view);
						return out;
					}
					if (ArrayBuffer.isView(value)) {
						const view = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
						const out = new __AgentOsWasiBuffer(view.length);
						out.set(view);
						return out;
					}
					const arr = Array.from(value || []);
					const out = new __AgentOsWasiBuffer(arr.length);
					for (let i = 0; i < arr.length; i += 1) out[i] = arr[i] & 0xff;
					return out;
				}
				static concat(list, totalLength) {
					const chunks = Array.from(list || []);
					if (totalLength === undefined) {
						totalLength = 0;
						for (const chunk of chunks) totalLength += chunk.length;
					}
					const out = new __AgentOsWasiBuffer(totalLength >>> 0);
					let offset = 0;
					for (const chunk of chunks) {
						if (offset >= out.length) break;
						const slice = offset + chunk.length > out.length ? chunk.subarray(0, out.length - offset) : chunk;
						out.set(slice, offset);
						offset += slice.length;
					}
					return out;
				}
				toString(encoding, start, end) {
					const view = this.subarray(start || 0, end === undefined ? this.length : end);
					if (encoding === "base64") {
						let binary = "";
						for (let i = 0; i < view.length; i += 1) binary += String.fromCharCode(view[i]);
						return btoa(binary);
					}
					if (encoding === "hex") {
						let hex = "";
						for (let i = 0; i < view.length; i += 1) hex += view[i].toString(16).padStart(2, "0");
						return hex;
					}
					return new TextDecoder().decode(view);
				}
			});
if (typeof globalThis !== "undefined" && typeof globalThis.__agentOSWasiModule === "undefined") {
  // Per-backend host seam (C / convergence): native populates it from its own
  // host globals (the \`|| __agentOs*\` fallbacks below); a non-native backend
  // (the browser converged worker) can pre-set \`globalThis.__agentOSWasiHost\`
  // with browser-provided equivalents so this same preview1 runner is shared.
  const __agentOSWasiHost =
    (typeof globalThis.__agentOSWasiHost === "object" &&
      globalThis.__agentOSWasiHost) ||
    {};
  const __agentOSWasiRequireBuiltin =
    __agentOSWasiHost.requireBuiltin ||
    (typeof __agentOSRequireBuiltin !== "undefined"
      ? __agentOSRequireBuiltin
      : (name) => globalThis.require(name));
  const __agentOSFs = () => __agentOSWasiRequireBuiltin("node:fs");
  const __agentOSPath = () => __agentOSWasiRequireBuiltin("node:path");
  const __agentOSCrypto = () => __agentOSWasiRequireBuiltin("node:crypto");
  // Stdio sync-RPC bridge + fd-handle lookup come from the host seam (a
  // non-native backend supplies browser equivalents); native falls back to its
  // own host globals so behavior is unchanged.
  // Lazy resolvers: the native host globals are populated AFTER this module is
  // defined (per-execution), so resolve at call time, not at module-load.
  const __agentOSWasiSyncRpc = () =>
    __agentOSWasiHost.syncRpc ||
    (typeof globalThis.__agentOSSyncRpc !== "undefined"
      ? globalThis.__agentOSSyncRpc
      : undefined);
  const __agentOSWasiLookupFdHandle = () =>
    __agentOSWasiHost.lookupFdHandle ||
    (typeof globalThis.lookupFdHandle === "function"
      ? globalThis.lookupFdHandle
      : undefined);
  const __agentOSWasiErrnoSuccess = 0;
  const __agentOSWasiErrnoAcces = 2;
  const __agentOSWasiErrnoBadf = 8;
  const __agentOSWasiErrnoExist = 20;
  const __agentOSWasiErrnoFault = 21;
  const __agentOSWasiErrnoInval = 28;
  const __agentOSWasiErrnoIo = 29;
  const __agentOSWasiErrnoNoent = 44;
  const __agentOSWasiErrnoNosys = 52;
  const __agentOSWasiErrnoNotdir = 54;
  const __agentOSWasiErrnoPipe = 64;
  const __agentOSWasiErrnoRofs = 69;
  const __agentOSWasiErrnoNotcapable = 76;
  const __agentOSWasiErrnoXdev = 18;
  const __agentOSWasiFiletypeUnknown = 0;
  const __agentOSWasiFiletypeCharacterDevice = 2;
  const __agentOSWasiFiletypeDirectory = 3;
  const __agentOSWasiFiletypeRegularFile = 4;
  const __agentOSWasiFiletypeSymbolicLink = 7;
  const __agentOSWasiLookupSymlinkFollow = 1;
  const __agentOSWasiOpenCreate = 1;
  const __agentOSWasiOpenDirectory = 2;
  const __agentOSWasiOpenExclusive = 4;
  const __agentOSWasiOpenTruncate = 8;
  const __agentOSWasiRightFdRead = 1n << 1n;
  const __agentOSWasiRightFdWrite = 1n << 6n;
  const __agentOSWasiDefaultRightsBase = 0xffffffffffffffffn;
  const __agentOSWasiDefaultRightsInheriting = 0xffffffffffffffffn;
  const __agentOSWasiWhenceSet = 0;
  const __agentOSWasiWhenceCur = 1;
  const __agentOSWasiWhenceEnd = 2;
  // Read cap: a non-native backend provides it via the seam; native uses its
  // build-substituted constant. The ternary short-circuits so the native-only
  // placeholder token is never evaluated when the seam supplies a number.
  const __agentOSWasmSyncReadLimitBytes =
    typeof __agentOSWasiHost.syncReadLimitBytes === "number"
      ? __agentOSWasiHost.syncReadLimitBytes
      : 16777216;
  const __agentOSKernelStdioSyncRpcEnabled = () =>
    process?.env?.AGENTOS_WASI_STDIO_SYNC_RPC === "1";
  const __agentOSWasiDebugEnabled = () => process?.env?.AGENTOS_WASM_WASI_DEBUG === "1";
  const __agentOSWasiDebug = (message) => {
    if (!__agentOSWasiDebugEnabled() || typeof process?.stderr?.write !== "function") {
      return;
    }
    try {
      process.stderr.write(\`[secure-exec-wasi] \${message}\\n\`);
    } catch {
      // Ignore debug logging failures.
    }
  };

  class WASI {
    constructor(options = {}) {
      this.args = Array.isArray(options.args) ? options.args.map((value) => String(value)) : [];
      this.env =
        options.env && typeof options.env === "object"
          ? Object.fromEntries(
              Object.entries(options.env).map(([key, value]) => [String(key), String(value)]),
            )
          : {};
      this.preopens = options.preopens && typeof options.preopens === "object" ? options.preopens : {};
      this.returnOnExit = options.returnOnExit === true;
      this.instance = null;
      this.nextFd = 3;
      this.fdTable = new Map([
        [0, { kind: "stdin", fdFlags: 0 }],
        [1, { kind: "stdout", fdFlags: 0 }],
        [2, { kind: "stderr", fdFlags: 0 }],
      ]);
      for (const [guestPath, spec] of Object.entries(this.preopens)) {
        const normalized = this._normalizePreopenSpec(spec);
        if (!normalized) {
          continue;
        }
        this.fdTable.set(this.nextFd++, {
          kind: "preopen",
          guestPath: String(guestPath),
          hostPath: normalized.hostPath,
          readOnly: normalized.readOnly,
          rightsBase: normalized.rightsBase,
          rightsInheriting: normalized.rightsInheriting,
          fdFlags: 0,
        });
      }
      this.wasiImport = {
        args_get: (...args) => this._argsGet(...args),
        args_sizes_get: (...args) => this._argsSizesGet(...args),
        clock_time_get: (...args) => this._clockTimeGet(...args),
        clock_res_get: (...args) => this._clockResGet(...args),
        environ_get: (...args) => this._environGet(...args),
        environ_sizes_get: (...args) => this._environSizesGet(...args),
        fd_close: (...args) => this._fdClose(...args),
        fd_fdstat_get: (...args) => this._fdFdstatGet(...args),
        fd_fdstat_set_flags: (...args) => this._fdFdstatSetFlags(...args),
        fd_filestat_get: (...args) => this._fdFilestatGet(...args),
        fd_filestat_set_size: (...args) => this._fdFilestatSetSize(...args),
        fd_prestat_dir_name: (...args) => this._fdPrestatDirName(...args),
        fd_prestat_get: (...args) => this._fdPrestatGet(...args),
        fd_pread: (...args) => this._fdPread(...args),
        fd_pwrite: (...args) => this._fdPwrite(...args),
        fd_readdir: (...args) => this._fdReaddir(...args),
        fd_read: (...args) => this._fdRead(...args),
        fd_seek: (...args) => this._fdSeek(...args),
        fd_sync: (...args) => this._fdSync(...args),
        fd_tell: (...args) => this._fdTell(...args),
        fd_write: (...args) => this._fdWrite(...args),
        path_create_directory: (...args) => this._pathCreateDirectory(...args),
        path_filestat_get: (...args) => this._pathFilestatGet(...args),
        path_link: (...args) => this._pathLink(...args),
        path_open: (...args) => this._pathOpen(...args),
        path_readlink: (...args) => this._pathReadlink(...args),
        path_remove_directory: (...args) => this._pathRemoveDirectory(...args),
        path_rename: (...args) => this._pathRename(...args),
        path_symlink: (...args) => this._pathSymlink(...args),
        path_unlink_file: (...args) => this._pathUnlinkFile(...args),
        poll_oneoff: (...args) => this._pollOneoff(...args),
        proc_exit: (...args) => this._procExit(...args),
        random_get: (...args) => this._randomGet(...args),
        sched_yield: (...args) => this._schedYield(...args),
      };
    }

    start(instance) {
      this.instance = instance;
      try {
        if (typeof instance?.exports?._start === "function") {
          instance.exports._start();
        }
        return 0;
      } catch (error) {
        if (error && error.__agentOSWasiExit === true) {
          return Number(error.code) >>> 0;
        }
        throw error;
      }
    }

    _memoryView() {
      const memory = this.instance?.exports?.memory;
      if (!(memory instanceof WebAssembly.Memory)) {
        throw new Error("WASI memory export is unavailable");
      }
      return new DataView(memory.buffer);
    }

    _memoryBytes() {
      const memory = this.instance?.exports?.memory;
      if (!(memory instanceof WebAssembly.Memory)) {
        throw new Error("WASI memory export is unavailable");
      }
      return new Uint8Array(memory.buffer);
    }

    _boundedIovLength(iovs, iovsLen) {
      const view = this._memoryView();
      let length = 0;
      for (let index = 0; index < (Number(iovsLen) >>> 0); index += 1) {
        const entryOffset = (Number(iovs) >>> 0) + index * 8;
        length += view.getUint32(entryOffset + 4, true);
        if (length > __agentOSWasmSyncReadLimitBytes) {
          throw new RangeError(
            \`WASI read iov length \${length} exceeds \${__agentOSWasmSyncReadLimitBytes}\`,
          );
        }
      }
      return length >>> 0;
    }

    // Read-side iov capacity, clamped (not thrown) to the sync read cap. A guest
    // may legitimately offer a huge read buffer (e.g. iov_len 0xffffffc0 = "read
    // up to ~4GB"); the runner reads only what is available, bounded by the cap,
    // so the read allocation/RPC stays bounded without rejecting the read. Writes
    // keep using _boundedIovLength (throwing) because their iov length is real
    // data that must not be silently truncated.
    _boundedReadLength(iovs, iovsLen) {
      const view = this._memoryView();
      let length = 0;
      for (let index = 0; index < (Number(iovsLen) >>> 0); index += 1) {
        const entryOffset = (Number(iovs) >>> 0) + index * 8;
        length += view.getUint32(entryOffset + 4, true);
        if (length >= __agentOSWasmSyncReadLimitBytes) {
          return __agentOSWasmSyncReadLimitBytes;
        }
      }
      return length >>> 0;
    }

    _normalizeRights(value, fallback) {
      try {
        return BigInt.asUintN(64, BigInt(value));
      } catch {
        return fallback;
      }
    }

    _normalizePreopenSpec(value) {
      // Path-model seam (convergence item C): native maps guest paths to HOST
      // paths (its preopen specs carry \`hostPath\`); a non-native backend with no
      // host paths (the browser, whose \`require("fs")\` IS the kernel VFS) can
      // supply \`__agentOSWasiHost.normalizePreopen\` to treat the guest/VFS path
      // as the "hostPath" identity, so the same runner serves both.
      if (typeof __agentOSWasiHost.normalizePreopen === "function") {
        const seamNormalized = __agentOSWasiHost.normalizePreopen(value, {
          defaultRightsBase: __agentOSWasiDefaultRightsBase,
          defaultRightsInheriting: __agentOSWasiDefaultRightsInheriting,
          normalizeRights: (rights, fallback) =>
            this._normalizeRights(rights, fallback),
        });
        return seamNormalized ?? null;
      }
      if (typeof value === "string") {
        return {
          hostPath: String(value),
          readOnly: false,
          rightsBase: __agentOSWasiDefaultRightsBase,
          rightsInheriting: __agentOSWasiDefaultRightsInheriting,
        };
      }
      if (!value || typeof value !== "object" || typeof value.hostPath !== "string") {
        return null;
      }
      return {
        hostPath: String(value.hostPath),
        readOnly: value.readOnly === true,
        rightsBase: this._normalizeRights(
          value.rightsBase,
          __agentOSWasiDefaultRightsBase,
        ),
        rightsInheriting: this._normalizeRights(
          value.rightsInheriting,
          __agentOSWasiDefaultRightsInheriting,
        ),
      };
    }

    _descriptorRightsBase(entry) {
      return this._normalizeRights(
        entry?.rightsBase,
        __agentOSWasiDefaultRightsBase,
      );
    }

    _descriptorRightsInheriting(entry) {
      return this._normalizeRights(
        entry?.rightsInheriting,
        __agentOSWasiDefaultRightsInheriting,
      );
    }

    _hasWriteRights(rights) {
      try {
        return (BigInt(rights) & __agentOSWasiRightFdWrite) !== 0n;
      } catch {
        return true;
      }
    }

    _writeUint32(ptr, value) {
      try {
        this._memoryView().setUint32(Number(ptr) >>> 0, Number(value) >>> 0, true);
        return __agentOSWasiErrnoSuccess;
      } catch {
        __agentOSWasiDebug(\`writeUint32 failed ptr=\${Number(ptr)} value=\${Number(value)}\`);
        return __agentOSWasiErrnoFault;
      }
    }

    _writeUint64(ptr, value) {
      try {
        this._memoryView().setBigUint64(Number(ptr) >>> 0, BigInt(value), true);
        return __agentOSWasiErrnoSuccess;
      } catch {
        __agentOSWasiDebug(\`writeUint64 failed ptr=\${Number(ptr)} value=\${String(value)}\`);
        return __agentOSWasiErrnoFault;
      }
    }

    _writeBytes(ptr, bytes) {
      try {
        this._memoryBytes().set(bytes, Number(ptr) >>> 0);
        return __agentOSWasiErrnoSuccess;
      } catch {
        __agentOSWasiDebug(\`writeBytes failed ptr=\${Number(ptr)} len=\${bytes?.length ?? 0}\`);
        return __agentOSWasiErrnoFault;
      }
    }

    _readBytes(ptr, len) {
      const start = Number(ptr) >>> 0;
      const end = start + (Number(len) >>> 0);
      return Buffer.from(this._memoryBytes().slice(start, end));
    }

    _readString(ptr, len) {
      return this._readBytes(ptr, len).toString("utf8");
    }

    _decodeSyncRpcBytes(value) {
      if (value == null) {
        return null;
      }
      if (typeof Buffer !== "undefined" && Buffer.isBuffer(value)) {
        return value;
      }
      if (value instanceof Uint8Array) {
        return Buffer.from(value);
      }
      if (ArrayBuffer.isView(value)) {
        return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
      }
      if (value instanceof ArrayBuffer) {
        return Buffer.from(value);
      }
      if (
        value &&
        typeof value === "object" &&
        value.__agentOSType === "bytes" &&
        typeof value.base64 === "string"
      ) {
        return Buffer.from(value.base64, "base64");
      }
      return null;
    }

    _dequeuePipeBytes(pipe, maxBytes) {
      if (!pipe || !Array.isArray(pipe.chunks) || pipe.chunks.length === 0) {
        return Buffer.alloc(0);
      }

      let remaining = Math.max(0, Number(maxBytes) >>> 0);
      if (remaining === 0) {
        return Buffer.alloc(0);
      }

      const parts = [];
      while (remaining > 0 && pipe.chunks.length > 0) {
        const chunk = pipe.chunks[0];
        if (!chunk || chunk.length === 0) {
          pipe.chunks.shift();
          continue;
        }

        if (chunk.length <= remaining) {
          parts.push(chunk);
          pipe.chunks.shift();
          remaining -= chunk.length;
          continue;
        }

        parts.push(chunk.subarray(0, remaining));
        pipe.chunks[0] = chunk.subarray(remaining);
        remaining = 0;
      }

      return Buffer.concat(parts);
    }

    _enqueuePipeBytes(pipe, bytes) {
      if (!pipe || !Array.isArray(pipe.chunks)) {
        return;
      }
      const chunk = Buffer.from(bytes ?? []);
      if (chunk.length === 0) {
        return;
      }
      pipe.chunks.push(chunk);
    }

    _pipeHasReaders(pipe) {
      return (
        (pipe?.readHandleCount ?? 0) > 0 ||
        (pipe?.consumers?.size ?? 0) > 0
      );
    }

    _flushPipeConsumers(pipe) {
      if (
        !pipe ||
        typeof pipe.consumers?.entries !== "function" ||
        !Array.isArray(pipe.chunks) ||
        pipe.chunks.length === 0 ||
        typeof globalThis?.__agentOSSyncRpc?.callSync !== "function"
      ) {
        return false;
      }

      let flushed = false;
      while (pipe.chunks.length > 0) {
        const chunk = pipe.chunks.shift();
        if (!chunk || chunk.length === 0) {
          continue;
        }

        for (const [consumerKey, consumer] of Array.from(pipe.consumers.entries())) {
          if (!consumer || typeof consumer.childId !== "string") {
            pipe.consumers.delete(consumerKey);
            continue;
          }
          try {
            __agentOSWasiSyncRpc().callSync("child_process.write_stdin", [
              consumer.childId,
              chunk,
            ]);
            flushed = true;
          } catch {
            pipe.consumers.delete(consumerKey);
          }
        }
      }

      return flushed;
    }

    _closePipeConsumers(pipe) {
      if (
        !pipe ||
        typeof pipe.consumers?.entries !== "function" ||
        typeof globalThis?.__agentOSSyncRpc?.callSync !== "function"
      ) {
        return false;
      }

      let closed = false;
      for (const [consumerKey, consumer] of Array.from(pipe.consumers.entries())) {
        if (!consumer || typeof consumer.childId !== "string") {
          pipe.consumers.delete(consumerKey);
          continue;
        }
        try {
          __agentOSWasiSyncRpc().callSync("child_process.close_stdin", [
            consumer.childId,
          ]);
          closed = true;
        } catch {
          // Ignore close errors during teardown.
        }
        pipe.consumers.delete(consumerKey);
      }

      return closed;
    }

    _pumpPipeProducers(pipe, waitMs) {
      if (
        !pipe ||
        typeof pipe.producers?.entries !== "function" ||
        typeof globalThis?.__agentOSSyncRpc?.callSync !== "function"
      ) {
        return false;
      }

      let processed = false;
      for (const [producerKey, producer] of Array.from(pipe.producers.entries())) {
        if (!producer || typeof producer.childId !== "string") {
          pipe.producers.delete(producerKey);
          continue;
        }

        let event = null;
        try {
          event = __agentOSWasiSyncRpc().callSync("child_process.poll", [
            producer.childId,
            Math.max(0, Number(waitMs) >>> 0),
          ]);
        } catch {
          pipe.producers.delete(producerKey);
          continue;
        }

        if (!event) {
          continue;
        }

        processed = true;
        const streamType =
          producer.stream === "stderr" ? "stderr" : producer.stream === "stdout" ? "stdout" : null;
        if ((event.type === "stdout" || event.type === "stderr") && event.type === streamType) {
          const chunk = this._decodeSyncRpcBytes(event.data);
          if (chunk && chunk.length > 0) {
            pipe.chunks.push(Buffer.from(chunk));
          }
          continue;
        }

        if (event.type === "exit") {
          pipe.producers.delete(producerKey);
          if (pipe.producers.size === 0 && (pipe.writeHandleCount ?? 0) === 0) {
            this._closePipeConsumers(pipe);
          }
          continue;
        }
      }

      return processed;
    }

    _collectIovs(iovs, iovsLen) {
      const totalLength = this._boundedIovLength(iovs, iovsLen);
      const view = this._memoryView();
      const chunks = [];
      for (let index = 0; index < (Number(iovsLen) >>> 0); index += 1) {
        const entryOffset = (Number(iovs) >>> 0) + index * 8;
        const ptr = view.getUint32(entryOffset, true);
        const len = view.getUint32(entryOffset + 4, true);
        chunks.push(this._readBytes(ptr, len));
      }
      return Buffer.concat(chunks, totalLength);
    }

    _writeToIovs(iovs, iovsLen, bytes) {
      const view = this._memoryView();
      const memory = this._memoryBytes();
      let sourceOffset = 0;
      for (let index = 0; index < (Number(iovsLen) >>> 0) && sourceOffset < bytes.length; index += 1) {
        const entryOffset = (Number(iovs) >>> 0) + index * 8;
        const ptr = view.getUint32(entryOffset, true);
        const len = view.getUint32(entryOffset + 4, true);
        const chunk = bytes.subarray(sourceOffset, sourceOffset + len);
        memory.set(chunk, Number(ptr) >>> 0);
        sourceOffset += chunk.length;
      }
      return sourceOffset;
    }

    _stringTable(values) {
      return values.map((value) => Buffer.from(\`\${String(value)}\\0\`, "utf8"));
    }

    _writeStringTable(values, offsetsPtr, bufferPtr) {
      try {
        const view = this._memoryView();
        const memory = this._memoryBytes();
        let cursor = Number(bufferPtr) >>> 0;
        for (let index = 0; index < values.length; index += 1) {
          const bytes = values[index];
          view.setUint32((Number(offsetsPtr) >>> 0) + index * 4, cursor, true);
          memory.set(bytes, cursor);
          cursor += bytes.length;
        }
        return __agentOSWasiErrnoSuccess;
      } catch {
        __agentOSWasiDebug(
          \`writeStringTable failed offsetsPtr=\${Number(offsetsPtr)} bufferPtr=\${Number(bufferPtr)} count=\${values.length}\`,
        );
        return __agentOSWasiErrnoFault;
      }
    }

    _filetypeForStats(stats) {
      if (!stats) {
        return __agentOSWasiFiletypeUnknown;
      }
      if (typeof stats.isDirectory === "function" && stats.isDirectory()) {
        return __agentOSWasiFiletypeDirectory;
      }
      if (typeof stats.isFile === "function" && stats.isFile()) {
        return __agentOSWasiFiletypeRegularFile;
      }
      if (typeof stats.isSymbolicLink === "function" && stats.isSymbolicLink()) {
        return __agentOSWasiFiletypeSymbolicLink;
      }
      if (typeof stats.isCharacterDevice === "function" && stats.isCharacterDevice()) {
        return __agentOSWasiFiletypeCharacterDevice;
      }
      return __agentOSWasiFiletypeUnknown;
    }

    _fdFiletype(entry) {
      if (!entry) {
        return __agentOSWasiFiletypeUnknown;
      }
      if (
        entry.kind === "stdin" ||
        entry.kind === "stdout" ||
        entry.kind === "stderr"
      ) {
        return __agentOSWasiFiletypeCharacterDevice;
      }
      if (entry.kind === "preopen" || entry.kind === "directory") {
        return __agentOSWasiFiletypeDirectory;
      }
      if (entry.kind === "symlink") {
        return __agentOSWasiFiletypeSymbolicLink;
      }
      return __agentOSWasiFiletypeRegularFile;
    }

    _mapFsError(error) {
      switch (error?.code) {
        case "EACCES":
        case "EPERM":
          return __agentOSWasiErrnoAcces;
        case "ENOENT":
          return __agentOSWasiErrnoNoent;
        case "ENOTDIR":
          return __agentOSWasiErrnoNotdir;
        case "EEXIST":
          return __agentOSWasiErrnoExist;
        case "EINVAL":
          return __agentOSWasiErrnoInval;
        case "EROFS":
          return __agentOSWasiErrnoRofs;
        case "EXDEV":
          return __agentOSWasiErrnoXdev;
        default:
          return __agentOSWasiErrnoIo;
      }
    }

    _descriptorEntry(fd) {
      return this.fdTable.get(Number(fd) >>> 0) ?? null;
    }

    _localFdHandle(fd) {
      // A non-native backend whose \`realFd\` values are not real host OS fds with
      // their own kernel offset (the browser, whose fs descriptors are a JS
      // handle table) disables local-fd passthrough so locally-opened files use
      // the offset-aware file branches (fd_read/fd_write pass the tracked
      // entry.offset as an explicit position) instead of host-passthrough reads
      // that rely on a null position advancing a real fd. Native keeps passthrough
      // so guest-opened fds can be shared with child processes.
      if (__agentOSWasiHost.disableLocalFdPassthrough === true) {
        return null;
      }
      const entry = this._descriptorEntry(fd);
      if (!entry || typeof entry.realFd !== "number") {
        return null;
      }
      return {
        kind: "host-passthrough",
        targetFd: entry.realFd,
        displayFd: Number(fd) >>> 0,
        refCount: 1,
        open: true,
        readOnly: entry.readOnly === true,
      };
    }

    _externalFdHandle(fd) {
      const descriptor = Number(fd) >>> 0;
      const localHandle = this._localFdHandle(descriptor);
      if (localHandle) {
        return localHandle;
      }
      try {
        if (typeof lookupFdHandle === "function") {
          return lookupFdHandle(descriptor) ?? null;
        }
      } catch {
        // Fall through to other lookup paths.
      }
      try {
        const __agentOSWasiFdHandleFn = __agentOSWasiLookupFdHandle();
        if (typeof __agentOSWasiFdHandleFn === "function") {
          return __agentOSWasiFdHandleFn(descriptor) ?? null;
        }
      } catch {
        // Ignore missing global bridge helpers.
      }
      return null;
    }

    _descriptorHostPath(entry) {
      if (!entry) {
        return null;
      }
      if (typeof entry.hostPath === "string") {
        return entry.hostPath;
      }
      if (typeof entry.realFd === "number") {
        return __agentOSFs().readlinkSync(\`/proc/self/fd/\${entry.realFd}\`);
      }
      return null;
    }

    _descriptorFsPath(entry) {
      if (!entry) {
        return null;
      }
      if (typeof entry.hostPath === "string" && entry.hostPath.length > 0) {
        return entry.hostPath;
      }
      if (typeof entry.guestPath === "string" && entry.guestPath.length > 0) {
        return entry.guestPath;
      }
      return null;
    }

    _sidecarManagedProcess() {
      if (
        typeof globalThis.__agentOSWasmInternalEnv?.AGENTOS_SANDBOX_ROOT ===
          "string" &&
        globalThis.__agentOSWasmInternalEnv.AGENTOS_SANDBOX_ROOT.length > 0
      ) {
        return true;
      }
      return (
        typeof process?.env?.AGENTOS_SANDBOX_ROOT === "string" &&
        process.env.AGENTOS_SANDBOX_ROOT.length > 0
      );
    }

    _descriptorDirectoryFsPath(entry) {
      if (
        (entry?.kind === "preopen" || entry?.kind === "directory") &&
        this._sidecarManagedProcess()
      ) {
        return this._descriptorGuestPath(entry);
      }
      return this._descriptorFsPath(entry);
    }

    _descriptorGuestPath(entry) {
      if (!entry) {
        return null;
      }
      const guestPath = typeof entry.guestPath === "string" ? entry.guestPath : null;
      if (guestPath === ".") {
        return this._currentGuestCwd();
      }
      if (typeof guestPath === "string" && guestPath.length > 0) {
        return __agentOSPath().posix.normalize(guestPath);
      }
      return null;
    }

    _descriptorPreopenName(entry) {
      if (!entry) {
        return null;
      }
      const guestPath = typeof entry.guestPath === "string" ? entry.guestPath : null;
      if (guestPath === ".") {
        return this._descriptorGuestPath(entry);
      }
      if (typeof guestPath === "string" && guestPath.length > 0) {
        return __agentOSPath().posix.normalize(guestPath);
      }
      return null;
    }

    _currentDirectoryPreopen() {
      for (const entry of this.fdTable.values()) {
        if (entry?.kind === "preopen" && entry.guestPath === ".") {
          return entry;
        }
      }
      return null;
    }

    _descriptorPathBase(entry, target) {
      const baseGuestPath = this._descriptorGuestPath(entry);
      if (typeof baseGuestPath !== "string") {
        return null;
      }
      return {
        entry,
        guestPath: baseGuestPath,
        hostPath: typeof entry?.hostPath === "string" ? entry.hostPath : null,
      };
    }

    _hostPathExists(hostPath) {
      try {
        __agentOSFs().statSync(hostPath);
        return true;
      } catch {
        return false;
      }
    }

    _currentGuestCwd() {
      const pwd =
        typeof this.env?.PWD === "string" && this.env.PWD.startsWith("/")
          ? this.env.PWD
          : typeof this.env?.HOME === "string" && this.env.HOME.startsWith("/")
            ? this.env.HOME
            : "/";
      return __agentOSPath().posix.normalize(pwd);
    }

    _resolveHostMappingForGuestPath(guestPath) {
      const normalized = __agentOSPath().posix.normalize(guestPath);
      const mappings = [];
      for (const entry of this.fdTable.values()) {
        if (entry?.kind !== "preopen" || typeof entry.hostPath !== "string") {
          continue;
        }
        const guestRoot = this._descriptorGuestPath(entry);
        if (typeof guestRoot !== "string") {
          continue;
        }
        mappings.push({
          guestRoot,
          hostPath: entry.hostPath,
          readOnly: entry.readOnly === true,
        });
      }
      mappings.sort((left, right) => right.guestRoot.length - left.guestRoot.length);

      for (const mapping of mappings) {
        const matchesRoot = mapping.guestRoot === "/" && normalized.startsWith("/");
        const matchesNested =
          normalized === mapping.guestRoot ||
          normalized.startsWith(\`\${mapping.guestRoot}/\`);
        if (!matchesRoot && !matchesNested) {
          continue;
        }
        const suffix =
          normalized === mapping.guestRoot
            ? ""
            : mapping.guestRoot === "/"
              ? normalized.slice(1)
              : normalized.slice(mapping.guestRoot.length + 1);
        return {
          hostPath: suffix
            ? __agentOSPath().join(mapping.hostPath, ...suffix.split("/"))
            : mapping.hostPath,
          readOnly: mapping.readOnly,
        };
      }

      return null;
    }

    _resolveHostPathForGuestPath(guestPath) {
      return this._resolveHostMappingForGuestPath(guestPath)?.hostPath ?? null;
    }

    _rootRelativeTargetPrefersCwd(target) {
      const normalizedTarget = __agentOSPath().posix.normalize(target || ".");
      if (normalizedTarget !== ".") {
        return false;
      }
      return !this._rootRelativeTargetMatchesAbsoluteArg(target);
    }

    _rootRelativeTargetMatchesAbsoluteArg(target) {
      const rootGuestPath = __agentOSPath().posix.resolve("/", target);
      return this.args
        .slice(1)
        .some(
          (arg) =>
            typeof arg === "string" &&
            arg.startsWith("/") &&
            __agentOSPath().posix.normalize(arg) === rootGuestPath,
        );
    }

    _resolveRootRelativePath(target, preferCreateParent = false) {
      const rootGuestPath = __agentOSPath().posix.resolve("/", target);
      const rootMapping = this._resolveHostMappingForGuestPath(rootGuestPath);
      const rootHostPath = rootMapping?.hostPath ?? null;
      const cwdGuestPath = this._currentGuestCwd();
      if (cwdGuestPath !== "/") {
        const cwdGuestTarget = __agentOSPath().posix.resolve(cwdGuestPath, target);
        const cwdMapping = this._resolveHostMappingForGuestPath(cwdGuestTarget);
        const cwdHostTarget = cwdMapping?.hostPath ?? null;
        if (
          typeof cwdHostTarget === "string" &&
          (
            (preferCreateParent && !this._rootRelativeTargetMatchesAbsoluteArg(target)) ||
            this._rootRelativeTargetPrefersCwd(target) ||
            (
              this._hostPathExists(cwdHostTarget) &&
              !(typeof rootHostPath === "string" && this._hostPathExists(rootHostPath))
            )
          )
        ) {
          return {
            guestPath: cwdGuestTarget,
            hostPath: cwdHostTarget,
            readOnly: cwdMapping?.readOnly === true,
          };
        }
      }
      return {
        guestPath: rootGuestPath,
        hostPath: rootHostPath,
        readOnly: rootMapping?.readOnly === true,
      };
    }

    _resolveDescriptorPath(fd, pathPtr, pathLen, options = {}) {
      const entry = this._descriptorEntry(fd);
      if (!entry) {
        return { error: __agentOSWasiErrnoBadf };
      }
      const target = this._readString(pathPtr, pathLen);
      const base = this._descriptorPathBase(entry, target);
      if (!base || typeof base.guestPath !== "string") {
        return { error: __agentOSWasiErrnoBadf };
      }
      const guestPath = target.startsWith("/")
        ? __agentOSPath().posix.normalize(target)
        : __agentOSPath().posix.resolve(base.guestPath, target);
      const mapped =
        base.guestPath === "/" && !target.startsWith("/")
          ? this._resolveRootRelativePath(
              target,
              options.preferCreateParent === true,
            )
          : {
              guestPath,
              ...(
                this._resolveHostMappingForGuestPath(guestPath) ??
                { hostPath: null, readOnly: false }
              ),
            };
      const hostPath = mapped.hostPath;
      if (typeof hostPath !== "string") {
        return { error: __agentOSWasiErrnoNoent };
      }
      return {
        error: __agentOSWasiErrnoSuccess,
        guestPath: mapped.guestPath,
        hostPath,
        readOnly: mapped.readOnly === true,
      };
    }

    _resolvedFsPath(resolved) {
      if (this._sidecarManagedProcess() && typeof resolved?.guestPath === "string") {
        return resolved.guestPath;
      }
      return resolved?.hostPath ?? null;
    }

    _writeFilestat(statPtr, stats, fallbackType) {
      try {
        const view = this._memoryView();
        const offset = Number(statPtr) >>> 0;
        const filetype = stats ? this._filetypeForStats(stats) : fallbackType;
        view.setBigUint64(offset, 0n, true);
        view.setBigUint64(offset + 8, BigInt(stats?.ino ?? 0), true);
        view.setUint8(offset + 16, filetype);
        view.setBigUint64(offset + 24, BigInt(stats?.nlink ?? 1), true);
        view.setBigUint64(offset + 32, BigInt(stats?.size ?? 0), true);
        view.setBigUint64(offset + 40, BigInt(Math.trunc((stats?.atimeMs ?? 0) * 1000000)), true);
        view.setBigUint64(offset + 48, BigInt(Math.trunc((stats?.mtimeMs ?? 0) * 1000000)), true);
        view.setBigUint64(offset + 56, BigInt(Math.trunc((stats?.ctimeMs ?? 0) * 1000000)), true);
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _argsSizesGet(argcPtr, argvBufSizePtr) {
      const values = this._stringTable(this.args);
      const total = values.reduce((sum, value) => sum + value.length, 0);
      const argcStatus = this._writeUint32(argcPtr, values.length);
      if (argcStatus !== __agentOSWasiErrnoSuccess) {
        return argcStatus;
      }
      return this._writeUint32(argvBufSizePtr, total);
    }

    _argsGet(argvPtr, argvBufPtr) {
      return this._writeStringTable(this._stringTable(this.args), argvPtr, argvBufPtr);
    }

    _environEntries() {
      return Object.entries(this.env).map(([key, value]) => \`\${key}=\${value}\`);
    }

    _environSizesGet(countPtr, bufSizePtr) {
      const values = this._stringTable(this._environEntries());
      const total = values.reduce((sum, value) => sum + value.length, 0);
      const countStatus = this._writeUint32(countPtr, values.length);
      if (countStatus !== __agentOSWasiErrnoSuccess) {
        return countStatus;
      }
      return this._writeUint32(bufSizePtr, total);
    }

    _environGet(environPtr, environBufPtr) {
      return this._writeStringTable(
        this._stringTable(this._environEntries()),
        environPtr,
        environBufPtr,
      );
    }

    _clockTimeGet(_clockId, _precision, resultPtr) {
      return this._writeUint64(resultPtr, BigInt(Date.now()) * 1000000n);
    }

    _clockResGet(_clockId, resultPtr) {
      return this._writeUint64(resultPtr, 1000000n);
    }

    _fdWrite(fd, iovs, iovsLen, nwrittenPtr) {
      try {
        const bytes = this._collectIovs(iovs, iovsLen);
        const descriptor = Number(fd) >>> 0;
        const handle = this._externalFdHandle(descriptor);
        if (handle?.kind === "pipe-write" && handle.pipe) {
          if (bytes.length > 0 && !this._pipeHasReaders(handle.pipe)) {
            return __agentOSWasiErrnoPipe;
          }
          this._enqueuePipeBytes(handle.pipe, bytes);
          this._flushPipeConsumers(handle.pipe);
          return this._writeUint32(nwrittenPtr, bytes.length);
        }
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {
          if (handle.readOnly === true) {
            return __agentOSWasiErrnoRofs;
          }
          if (descriptor === 1 || descriptor === 2) {
            const sidecarManagedProcess =
              typeof process?.env?.AGENTOS_SANDBOX_ROOT === "string" &&
              process.env.AGENTOS_SANDBOX_ROOT.length > 0;
            const useKernelStdioSyncRpc =
              sidecarManagedProcess || __agentOSKernelStdioSyncRpcEnabled();
            if (useKernelStdioSyncRpc) {
              const written = Number(
                __agentOSWasiSyncRpc().callSync("__kernel_stdio_write", [descriptor, bytes]),
              ) >>> 0;
              return this._writeUint32(nwrittenPtr, written);
            }
          }
          const written = __agentOSFs().writeSync(
            handle.targetFd,
            bytes,
            0,
            bytes.length,
            null,
          );
          return this._writeUint32(nwrittenPtr, written);
        }
        if (handle?.kind === "guest-file" && typeof handle.targetFd === "number") {
          const position = handle.append ? null : (handle.position ?? 0);
          const written = __agentOSFs().writeSync(
            handle.targetFd,
            bytes,
            0,
            bytes.length,
            position,
          );
          if (handle.append) {
            handle.position = Number(__agentOSFs().fstatSync(handle.targetFd).size ?? 0);
          } else {
            handle.position = (handle.position ?? 0) + written;
          }
          return this._writeUint32(nwrittenPtr, written);
        }
        if (handle?.kind === "stdio" && typeof handle.targetFd === "number") {
          const targetFd = Number(handle.targetFd) >>> 0;
          if (targetFd === 1 || targetFd === 2) {
            const sidecarManagedProcess =
              typeof process?.env?.AGENTOS_SANDBOX_ROOT === "string" &&
              process.env.AGENTOS_SANDBOX_ROOT.length > 0;
            const useKernelStdioSyncRpc =
              sidecarManagedProcess || __agentOSKernelStdioSyncRpcEnabled();
            const written = useKernelStdioSyncRpc
              ? Number(__agentOSWasiSyncRpc().callSync("__kernel_stdio_write", [targetFd, bytes])) >>> 0
              : (targetFd === 2 ? process.stderr.write(bytes) : process.stdout.write(bytes), bytes.length);
            return this._writeUint32(nwrittenPtr, written);
          }
          return __agentOSWasiErrnoBadf;
        }
        const entry = this.fdTable.get(descriptor);
        if (!entry) {
          return __agentOSWasiErrnoBadf;
        }
        if (entry.kind === "stdout") {
          const sidecarManagedProcess =
            typeof process?.env?.AGENTOS_SANDBOX_ROOT === "string" &&
            process.env.AGENTOS_SANDBOX_ROOT.length > 0;
          const useKernelStdioSyncRpc =
            sidecarManagedProcess || __agentOSKernelStdioSyncRpcEnabled();
          const written = useKernelStdioSyncRpc
            ? Number(__agentOSWasiSyncRpc().callSync("__kernel_stdio_write", [1, bytes])) >>> 0
            : (process.stdout.write(bytes), bytes.length);
          return this._writeUint32(nwrittenPtr, written);
        }
        if (entry.kind === "stderr") {
          const sidecarManagedProcess =
            typeof process?.env?.AGENTOS_SANDBOX_ROOT === "string" &&
            process.env.AGENTOS_SANDBOX_ROOT.length > 0;
          const useKernelStdioSyncRpc =
            sidecarManagedProcess || __agentOSKernelStdioSyncRpcEnabled();
          const written = useKernelStdioSyncRpc
            ? Number(__agentOSWasiSyncRpc().callSync("__kernel_stdio_write", [2, bytes])) >>> 0
            : (process.stderr.write(bytes), bytes.length);
          return this._writeUint32(nwrittenPtr, written);
        }
        if (entry.readOnly === true) {
          return __agentOSWasiErrnoRofs;
        }
        if (entry.kind === "file") {
          const position = typeof entry.offset === "number" ? entry.offset : null;
          const written = __agentOSFs().writeSync(
            entry.realFd,
            bytes,
            0,
            bytes.length,
            position,
          );
          if (typeof entry.offset === "number") {
            entry.offset += written;
          }
          return this._writeUint32(nwrittenPtr, written);
        }
        return __agentOSWasiErrnoBadf;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _fdPwrite(fd, iovs, iovsLen, offset, nwrittenPtr) {
      try {
        const bytes = this._collectIovs(iovs, iovsLen);
        const descriptor = Number(fd) >>> 0;
        const handle = this._externalFdHandle(descriptor);
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {
          if (handle.readOnly === true) {
            return __agentOSWasiErrnoRofs;
          }
          const written = __agentOSFs().writeSync(
            handle.targetFd,
            bytes,
            0,
            bytes.length,
            Number(offset) >>> 0,
          );
          return this._writeUint32(nwrittenPtr, written);
        }
        const entry = this.fdTable.get(descriptor);
        if (!entry || entry.kind !== "file") {
          return __agentOSWasiErrnoBadf;
        }
        if (entry.readOnly === true) {
          return __agentOSWasiErrnoRofs;
        }
        const written = __agentOSFs().writeSync(
          entry.realFd,
          bytes,
          0,
          bytes.length,
          Number(offset) >>> 0,
        );
        return this._writeUint32(nwrittenPtr, written);
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdPread(fd, iovs, iovsLen, offset, nreadPtr) {
      try {
        const descriptor = Number(fd) >>> 0;
        const explicitOffset = Number(offset) >>> 0;
        const totalLength = this._boundedReadLength(iovs, iovsLen);
        const buffer = Buffer.alloc(totalLength);
        const handle = this._externalFdHandle(descriptor);
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {
          const bytesRead = __agentOSFs().readSync(
            handle.targetFd,
            buffer,
            0,
            totalLength,
            explicitOffset,
          );
          const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
          return this._writeUint32(nreadPtr, written);
        }
        const entry = this.fdTable.get(descriptor);
        if (!entry || entry.kind !== "file") {
          return __agentOSWasiErrnoBadf;
        }
        const bytesRead = __agentOSFs().readSync(
          entry.realFd,
          buffer,
          0,
          totalLength,
          explicitOffset,
        );
        const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
        return this._writeUint32(nreadPtr, written);
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdRead(fd, iovs, iovsLen, nreadPtr) {
      try {
        const descriptor = Number(fd) >>> 0;
        const handle = this._externalFdHandle(descriptor);
        if (handle?.kind === "pipe-read" && handle.pipe) {
          const totalLength = this._boundedReadLength(iovs, iovsLen);
          while (handle.pipe.chunks.length === 0) {
            if (handle.pipe.writeHandleCount === 0 && handle.pipe.producers.size === 0) {
              return this._writeUint32(nreadPtr, 0);
            }
            this._pumpPipeProducers(handle.pipe, 10);
          }
          const chunk = this._dequeuePipeBytes(handle.pipe, totalLength);
          const written = this._writeToIovs(iovs, iovsLen, chunk);
          return this._writeUint32(nreadPtr, written);
        }
        if (handle?.kind === "stdio" && Number(handle.targetFd) === 0) {
          const totalLength = this._boundedReadLength(iovs, iovsLen);
          if (typeof __agentOSWasiHost.readStdin === "function") {
            const value = __agentOSWasiHost.readStdin(totalLength);
            if (value == null) {
              return this._writeUint32(nreadPtr, 0);
            }
            const chunk =
              typeof value === "string"
                ? Buffer.from(value, "utf8")
                : value instanceof Uint8Array
                  ? value
                  : Buffer.from(value);
            if (chunk.length === 0) {
              return this._writeUint32(nreadPtr, 0);
            }
            const written = this._writeToIovs(iovs, iovsLen, chunk);
            return this._writeUint32(nreadPtr, written);
          }
          const buffer = Buffer.alloc(totalLength);
          const bytesRead = __agentOSFs().readSync(0, buffer, 0, totalLength, null);
          const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
          return this._writeUint32(nreadPtr, written);
        }
        const entry = this.fdTable.get(descriptor);
        if (!entry) {
          return __agentOSWasiErrnoBadf;
        }
        if (entry.kind === "stdin") {
          const totalLength = this._boundedReadLength(iovs, iovsLen);
          const syncRpc =
            typeof globalThis?.__agentOSSyncRpc?.callSync === "function"
              ? __agentOSWasiSyncRpc()
              : null;
          const sidecarManagedProcess =
            typeof process?.env?.AGENTOS_SANDBOX_ROOT === "string" &&
            process.env.AGENTOS_SANDBOX_ROOT.length > 0;
          if (syncRpc && (sidecarManagedProcess || __agentOSKernelStdioSyncRpcEnabled())) {
            try {
              let chunk = null;
              while (true) {
                const response = syncRpc.callSync("__kernel_stdin_read", [totalLength, 10]);
                if (
                  response &&
                  typeof response === "object" &&
                  typeof response.dataBase64 === "string"
                ) {
                  chunk = Buffer.from(response.dataBase64, "base64");
                  break;
                }
                if (response && typeof response === "object" && response.done === true) {
                  chunk = Buffer.alloc(0);
                  break;
                }
                if (
                  typeof Atomics?.wait === "function" &&
                  typeof syntheticWaitArray !== "undefined"
                ) {
                  Atomics.wait(syntheticWaitArray, 0, 0, 10);
                }
              }
              if (!chunk || chunk.length === 0) {
                return this._writeUint32(nreadPtr, 0);
              }
              const written = this._writeToIovs(iovs, iovsLen, chunk);
              return this._writeUint32(nreadPtr, written);
            } catch {
              // Fall back to direct stdin reads when the sync bridge is unavailable
              // in the standalone runner bootstrap.
            }
          }
          // Host-seam stdin (a non-native backend whose stdin is delivered through
          // the runtime process object, not a kernel fd): read the queued bytes
          // directly instead of fs.readSync on a descriptor the JS fs table does
          // not own.
          if (typeof __agentOSWasiHost.readStdin === "function") {
            const value = __agentOSWasiHost.readStdin(totalLength);
            if (value == null) {
              return this._writeUint32(nreadPtr, 0);
            }
            const chunk =
              typeof value === "string"
                ? Buffer.from(value, "utf8")
                : value instanceof Uint8Array
                  ? value
                  : Buffer.from(value);
            if (chunk.length === 0) {
              return this._writeUint32(nreadPtr, 0);
            }
            const written = this._writeToIovs(iovs, iovsLen, chunk);
            return this._writeUint32(nreadPtr, written);
          }
          const buffer = Buffer.alloc(totalLength);
          const directStdinFd =
            (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
            typeof handle.targetFd === "number"
              ? handle.targetFd
              : typeof process?.stdin?.fd === "number"
                ? process.stdin.fd
                : 0;
          const bytesRead = __agentOSFs().readSync(
            directStdinFd,
            buffer,
            0,
            totalLength,
            null,
          );
          const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
          return this._writeUint32(nreadPtr, written);
        }
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {
          const totalLength = this._boundedReadLength(iovs, iovsLen);
          const buffer = Buffer.alloc(totalLength);
          const bytesRead = __agentOSFs().readSync(
            handle.targetFd,
            buffer,
            0,
            totalLength,
            null,
          );
          const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
          return this._writeUint32(nreadPtr, written);
        }
        if (entry.kind !== "file") {
          return __agentOSWasiErrnoBadf;
        }
        // WASI rights: a descriptor opened without FD_READ cannot be read.
        if (
          typeof entry.rightsBase === "bigint" &&
          (entry.rightsBase & __agentOSWasiRightFdRead) === 0n
        ) {
          return __agentOSWasiErrnoNotcapable;
        }
        const totalLength = this._boundedReadLength(iovs, iovsLen);
        const buffer = Buffer.alloc(totalLength);
        const position = typeof entry.offset === "number" ? entry.offset : null;
        const bytesRead = __agentOSFs().readSync(
          entry.realFd,
          buffer,
          0,
          totalLength,
          position,
        );
        if (typeof entry.offset === "number") {
          entry.offset += bytesRead;
        }
        const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
        return this._writeUint32(nreadPtr, written);
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _fdClose(fd) {
      try {
        const descriptor = Number(fd) >>> 0;
        const handle = this._externalFdHandle(descriptor);
        if (handle?.kind === "pipe-read" && handle.pipe) {
          handle.open = false;
          handle.pipe.readHandleCount = Math.max(0, (handle.pipe.readHandleCount ?? 0) - 1);
          if (typeof handle.onClose === "function") {
            handle.onClose(handle, descriptor);
          }
          return __agentOSWasiErrnoSuccess;
        }
        if (handle?.kind === "pipe-write" && handle.pipe) {
          handle.open = false;
          handle.pipe.writeHandleCount = Math.max(0, (handle.pipe.writeHandleCount ?? 0) - 1);
          if (typeof handle.onClose === "function") {
            handle.onClose(handle, descriptor);
          }
          return __agentOSWasiErrnoSuccess;
        }
        if (handle?.kind === "guest-file" || handle?.kind === "stdio") {
          handle.open = false;
          return __agentOSWasiErrnoSuccess;
        }
        const entry = this.fdTable.get(descriptor);
        if (!entry) {
          return __agentOSWasiErrnoBadf;
        }
        const retainedDelegateRefs = (() => {
          try {
            if (typeof globalThis.__agentOSWasiDelegateFdRefCount === "function") {
              return Number(globalThis.__agentOSWasiDelegateFdRefCount(descriptor)) || 0;
            }
          } catch {
            // Fall through to the default close path.
          }
          return 0;
        })();
        if (entry.kind === "file" && retainedDelegateRefs <= 0) {
          __agentOSFs().closeSync(entry.realFd);
        }
        if (descriptor > 2 && retainedDelegateRefs <= 0) {
          this.fdTable.delete(descriptor);
        }
        return __agentOSWasiErrnoSuccess;
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdSync(fd) {
      try {
        const descriptor = Number(fd) >>> 0;
        const handle = this._externalFdHandle(descriptor);
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {
          __agentOSFs().fsyncSync(handle.targetFd);
          return __agentOSWasiErrnoSuccess;
        }
        const entry = this.fdTable.get(descriptor);
        if (!entry) {
          return __agentOSWasiErrnoBadf;
        }
        // fsync on a stdio stream (stdin/stdout/stderr) is a no-op success; only
        // descriptors with a real backing fd are flushed.
        if (
          entry.kind === "stdin" ||
          entry.kind === "stdout" ||
          entry.kind === "stderr"
        ) {
          return __agentOSWasiErrnoSuccess;
        }
        if (entry.kind !== "file" || typeof entry.realFd !== "number") {
          return __agentOSWasiErrnoBadf;
        }
        __agentOSFs().fsyncSync(entry.realFd);
        return __agentOSWasiErrnoSuccess;
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdFdstatGet(fd, statPtr) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry) {
          return __agentOSWasiErrnoBadf;
        }
        const view = this._memoryView();
        const offset = Number(statPtr) >>> 0;
        view.setUint8(offset, this._fdFiletype(entry));
        view.setUint16(offset + 2, (Number(entry.fdFlags) >>> 0) & 0xffff, true);
        view.setBigUint64(offset + 8, this._descriptorRightsBase(entry), true);
        view.setBigUint64(offset + 16, this._descriptorRightsInheriting(entry), true);
        return __agentOSWasiErrnoSuccess;
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdFdstatSetFlags(fd, flags) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry) {
          return __agentOSWasiErrnoBadf;
        }
        entry.fdFlags = (Number(flags) >>> 0) & 0xffff;
        return __agentOSWasiErrnoSuccess;
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdFilestatGet(fd, statPtr) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry) {
          return __agentOSWasiErrnoBadf;
        }
        if (
          entry.kind === "stdin" ||
          entry.kind === "stdout" ||
          entry.kind === "stderr"
        ) {
          return this._writeFilestat(statPtr, null, __agentOSWasiFiletypeCharacterDevice);
        }
        if (entry.kind === "preopen") {
          const stats = __agentOSFs().statSync(entry.guestPath);
          return this._writeFilestat(statPtr, stats, __agentOSWasiFiletypeDirectory);
        }
        const stats =
          typeof entry.realFd === "number"
            ? __agentOSFs().fstatSync(entry.realFd)
            : __agentOSFs().statSync(this._descriptorFsPath(entry));
        return this._writeFilestat(statPtr, stats, this._fdFiletype(entry));
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _fdFilestatSetSize(fd, size) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "file" || typeof entry.realFd !== "number") {
          return __agentOSWasiErrnoBadf;
        }
        if (entry.readOnly === true) {
          return __agentOSWasiErrnoRofs;
        }
        __agentOSFs().ftruncateSync(entry.realFd, Number(size));
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _fdSeek(fd, offset, whence, newOffsetPtr) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "file" || typeof entry.realFd !== "number") {
          return __agentOSWasiErrnoBadf;
        }
        const delta = Number(offset);
        if (!Number.isFinite(delta)) {
          return __agentOSWasiErrnoInval;
        }
        const currentOffset = typeof entry.offset === "number" ? entry.offset : 0;
        let nextOffset = 0;
        switch (Number(whence) >>> 0) {
          case __agentOSWasiWhenceSet:
            nextOffset = delta;
            break;
          case __agentOSWasiWhenceCur:
            nextOffset = currentOffset + delta;
            break;
          case __agentOSWasiWhenceEnd: {
            const stats = __agentOSFs().fstatSync(entry.realFd);
            nextOffset = Number(stats?.size ?? 0) + delta;
            break;
          }
          default:
            return __agentOSWasiErrnoInval;
        }
        if (!Number.isFinite(nextOffset) || nextOffset < 0) {
          return __agentOSWasiErrnoInval;
        }
        entry.offset = nextOffset;
        return this._writeUint64(newOffsetPtr, BigInt(nextOffset));
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _fdTell(fd, offsetPtr) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "file") {
          return __agentOSWasiErrnoBadf;
        }
        const offset = typeof entry.offset === "number" ? entry.offset : 0;
        return this._writeUint64(offsetPtr, BigInt(offset));
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _fdPrestatGet(fd, prestatPtr) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "preopen") {
          return __agentOSWasiErrnoBadf;
        }
        const guestPath = this._descriptorPreopenName(entry);
        if (typeof guestPath !== "string") {
          return __agentOSWasiErrnoBadf;
        }
        const view = this._memoryView();
        const offset = Number(prestatPtr) >>> 0;
        view.setUint8(offset, 0);
        view.setUint32(offset + 4, Buffer.byteLength(guestPath), true);
        return __agentOSWasiErrnoSuccess;
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdPrestatDirName(fd, pathPtr, pathLen) {
      try {
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "preopen") {
          return __agentOSWasiErrnoBadf;
        }
        const guestPath = this._descriptorPreopenName(entry);
        if (typeof guestPath !== "string") {
          return __agentOSWasiErrnoBadf;
        }
        const bytes = Buffer.from(guestPath, "utf8");
        if ((Number(pathLen) >>> 0) < bytes.length) {
          return __agentOSWasiErrnoFault;
        }
        return this._writeBytes(pathPtr, bytes);
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _fdReaddir(fd, bufPtr, bufLen, cookie, bufUsedPtr) {
      try {
        const entry = this._descriptorEntry(fd);
        const fsPath = this._descriptorDirectoryFsPath(entry);
        if (
          !entry ||
          (entry.kind !== "preopen" && entry.kind !== "directory") ||
          typeof fsPath !== "string"
        ) {
          return __agentOSWasiErrnoBadf;
        }
        const dirents = __agentOSFs()
          .readdirSync(fsPath, { withFileTypes: true })
          .sort((left, right) => left.name.localeCompare(right.name));
        const view = this._memoryView();
        const memory = this._memoryBytes();
        let offset = Number(bufPtr) >>> 0;
        const limit = offset + (Number(bufLen) >>> 0);
        let used = 0;
        for (let index = Number(cookie) >>> 0; index < dirents.length; index += 1) {
          const dirent = dirents[index];
          const nameBytes = Buffer.from(dirent.name, "utf8");
          const recordLen = 24 + nameBytes.length;
          if (offset + recordLen > limit) {
            break;
          }
          view.setBigUint64(offset, BigInt(index + 1), true);
          view.setBigUint64(offset + 8, BigInt(index + 1), true);
          view.setUint32(offset + 16, nameBytes.length, true);
          view.setUint8(
            offset + 20,
            dirent.isDirectory()
              ? __agentOSWasiFiletypeDirectory
              : dirent.isSymbolicLink()
                ? __agentOSWasiFiletypeSymbolicLink
                : __agentOSWasiFiletypeRegularFile,
          );
          memory.set(nameBytes, offset + 24);
          offset += recordLen;
          used += recordLen;
        }
        return this._writeUint32(bufUsedPtr, used);
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathCreateDirectory(fd, pathPtr, pathLen) {
      try {
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOSWasiErrnoSuccess) {
          return resolved.error;
        }
        if (resolved.readOnly) {
          return __agentOSWasiErrnoRofs;
        }
        __agentOSFs().mkdirSync(this._resolvedFsPath(resolved));
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathLink(oldFd, _oldFlags, oldPathPtr, oldPathLen, newFd, newPathPtr, newPathLen) {
      try {
        const source = this._resolveDescriptorPath(oldFd, oldPathPtr, oldPathLen);
        if (source.error !== __agentOSWasiErrnoSuccess) {
          return source.error;
        }
        const destination = this._resolveDescriptorPath(newFd, newPathPtr, newPathLen);
        if (destination.error !== __agentOSWasiErrnoSuccess) {
          return destination.error;
        }
        if (source.readOnly || destination.readOnly) {
          return __agentOSWasiErrnoRofs;
        }
        __agentOSFs().linkSync(this._resolvedFsPath(source), this._resolvedFsPath(destination));
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathOpen(fd, _dirflags, pathPtr, pathLen, oflags, rightsBase, rightsInheriting, _fdflags, openedFdPtr) {
      try {
        const entry = this._descriptorEntry(fd);
        if (
          !entry ||
          (entry.kind !== "preopen" && entry.kind !== "directory") ||
          typeof entry.hostPath !== "string"
        ) {
          return __agentOSWasiErrnoBadf;
        }
        const requestedFlags = Number(oflags) >>> 0;
        const createOrTruncate =
          (requestedFlags & __agentOSWasiOpenCreate) !== 0 ||
          (requestedFlags & __agentOSWasiOpenTruncate) !== 0;
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen, {
          preferCreateParent: createOrTruncate,
        });
        if (resolved.error !== __agentOSWasiErrnoSuccess) {
          return resolved.error;
        }
        const guestPath = resolved.guestPath;
        const fsPath = this._resolvedFsPath(resolved);
        const openDirectory = (requestedFlags & __agentOSWasiOpenDirectory) !== 0;
        const allowedRightsBase = this._descriptorRightsBase(entry);
        const allowedRightsInheriting = this._descriptorRightsInheriting(entry);
        const requestedRightsBase = this._normalizeRights(rightsBase, allowedRightsInheriting);
        const requestedRightsInheriting = this._normalizeRights(
          rightsInheriting,
          allowedRightsInheriting,
        );
        if (
          (requestedRightsBase & ~allowedRightsInheriting) !== 0n ||
          (requestedRightsInheriting & ~allowedRightsInheriting) !== 0n
        ) {
          return __agentOSWasiErrnoAcces;
        }
        const requestedWriteAccess =
          !openDirectory &&
          (createOrTruncate || this._hasWriteRights(requestedRightsBase));
        if (
          requestedWriteAccess &&
          !this._hasWriteRights(allowedRightsBase)
        ) {
          return __agentOSWasiErrnoAcces;
        }
        if (requestedWriteAccess && resolved.readOnly) {
          return __agentOSWasiErrnoRofs;
        }
        const fsConstants = __agentOSFs().constants ?? {};
        let openFlags = requestedWriteAccess
          ? fsConstants.O_RDWR ?? 2
          : fsConstants.O_RDONLY ?? 0;
        if ((requestedFlags & __agentOSWasiOpenCreate) !== 0) {
          openFlags |= fsConstants.O_CREAT ?? 64;
        }
        if ((requestedFlags & __agentOSWasiOpenExclusive) !== 0) {
          openFlags |= fsConstants.O_EXCL ?? 128;
        }
        if ((requestedFlags & __agentOSWasiOpenTruncate) !== 0) {
          openFlags |= fsConstants.O_TRUNC ?? 512;
        }
        if (openDirectory) {
          openFlags |= fsConstants.O_DIRECTORY ?? 0;
        }
        if (createOrTruncate && !openDirectory) {
          __agentOSFs().statSync(__agentOSPath().dirname(fsPath));
        } else {
          __agentOSFs().statSync(fsPath);
        }
        const realFd = __agentOSFs().openSync(fsPath, openFlags);
        const stats =
          createOrTruncate && !openDirectory
            ? __agentOSFs().fstatSync(realFd)
            : __agentOSFs().statSync(fsPath);
        const openedFd = this.nextFd++;
        this.fdTable.set(openedFd, {
          kind: stats.isDirectory() ? "directory" : "file",
          guestPath,
          hostPath: fsPath,
          readOnly: resolved.readOnly === true,
          realFd,
          offset: 0,
          rightsBase: requestedRightsBase & allowedRightsInheriting,
          rightsInheriting: requestedRightsInheriting & allowedRightsInheriting,
          fdFlags: (Number(_fdflags) >>> 0) & 0xffff,
        });
        return this._writeUint32(openedFdPtr, openedFd);
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathSymlink(targetPtr, targetLen, fd, pathPtr, pathLen) {
      try {
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOSWasiErrnoSuccess) {
          return resolved.error;
        }
        if (resolved.readOnly) {
          return __agentOSWasiErrnoRofs;
        }
        const target = this._readString(targetPtr, targetLen);
        __agentOSFs().symlinkSync(target, this._resolvedFsPath(resolved));
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathRemoveDirectory(fd, pathPtr, pathLen) {
      try {
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOSWasiErrnoSuccess) {
          return resolved.error;
        }
        if (resolved.readOnly) {
          return __agentOSWasiErrnoRofs;
        }
        __agentOSFs().rmdirSync(this._resolvedFsPath(resolved));
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathRename(oldFd, oldPathPtr, oldPathLen, newFd, newPathPtr, newPathLen) {
      try {
        const source = this._resolveDescriptorPath(oldFd, oldPathPtr, oldPathLen);
        if (source.error !== __agentOSWasiErrnoSuccess) {
          return source.error;
        }
        const destination = this._resolveDescriptorPath(newFd, newPathPtr, newPathLen);
        if (destination.error !== __agentOSWasiErrnoSuccess) {
          return destination.error;
        }
        if (source.readOnly || destination.readOnly) {
          return __agentOSWasiErrnoRofs;
        }
        __agentOSFs().renameSync(this._resolvedFsPath(source), this._resolvedFsPath(destination));
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathUnlinkFile(fd, pathPtr, pathLen) {
      try {
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOSWasiErrnoSuccess) {
          return resolved.error;
        }
        if (resolved.readOnly) {
          return __agentOSWasiErrnoRofs;
        }
        __agentOSFs().unlinkSync(this._resolvedFsPath(resolved));
        return __agentOSWasiErrnoSuccess;
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathFilestatGet(fd, flags, pathPtr, pathLen, statPtr) {
      try {
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOSWasiErrnoSuccess) {
          return resolved.error;
        }
        const follow = (Number(flags) & __agentOSWasiLookupSymlinkFollow) !== 0;
        const stats = follow
          ? __agentOSFs().statSync(this._resolvedFsPath(resolved))
          : __agentOSFs().lstatSync(this._resolvedFsPath(resolved));
        return this._writeFilestat(statPtr, stats, this._filetypeForStats(stats));
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pathReadlink(fd, pathPtr, pathLen, bufPtr, bufLen, bufUsedPtr) {
      try {
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOSWasiErrnoSuccess) {
          return resolved.error;
        }
        const bytes = Buffer.from(__agentOSFs().readlinkSync(resolved.guestPath), "utf8");
        const length = Math.min(bytes.length, Number(bufLen) >>> 0);
        const writeStatus = this._writeBytes(bufPtr, bytes.subarray(0, length));
        if (writeStatus !== __agentOSWasiErrnoSuccess) {
          return writeStatus;
        }
        return this._writeUint32(bufUsedPtr, length);
      } catch (error) {
        return this._mapFsError(error);
      }
    }

    _pollOneoff(inPtr, outPtr, nsubscriptions, neventsPtr) {
      try {
        const subscriptionCount = Number(nsubscriptions) >>> 0;
        if (subscriptionCount === 0) {
          return this._writeUint32(neventsPtr, 0);
        }

        const subscriptionSize = 48;
        const eventSize = 32;
        const kernelPollIn = 0x0001;
        const kernelPollOut = 0x0004;
        const kernelPollErr = 0x0008;
        const kernelPollHup = 0x0010;
        const view = this._memoryView();
        const memory = this._memoryBytes();
        const syncRpc =
          typeof globalThis?.__agentOSSyncRpc?.callSync === "function"
            ? __agentOSWasiSyncRpc()
            : null;
        const subscriptions = [];
        let timeoutMs = null;

        for (let index = 0; index < subscriptionCount; index += 1) {
          const base = (Number(inPtr) >>> 0) + index * subscriptionSize;
          const tag = view.getUint8(base + 8);
          const userdata = memory.slice(base, base + 8);
          if (tag === 0) {
            const timeoutNs = view.getBigUint64(base + 24, true);
            const relativeTimeoutMs = Number(timeoutNs / 1000000n);
            timeoutMs =
              timeoutMs == null ? relativeTimeoutMs : Math.min(timeoutMs, relativeTimeoutMs);
            subscriptions.push({ kind: "clock", userdata });
            continue;
          }

          if (tag !== 1 && tag !== 2) {
            subscriptions.push({ kind: "unsupported", userdata });
            continue;
          }

          const fd = view.getUint32(base + 16, true);
          const descriptor = Number(fd) >>> 0;
          const handle = this._externalFdHandle(descriptor);
          const entry = this._descriptorEntry(descriptor);
          let targetFd = null;
          if (
            (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
            typeof handle.targetFd === "number"
          ) {
            targetFd = Number(handle.targetFd) >>> 0;
          } else if (
            entry?.kind === "stdin" ||
            entry?.kind === "stdout" ||
            entry?.kind === "stderr"
          ) {
            targetFd = descriptor;
          }

          subscriptions.push({
            kind: tag === 1 ? "fd_read" : "fd_write",
            fd: descriptor,
            handle,
            targetFd,
            streamKind: entry?.kind,
            userdata,
          });
        }

        const deadline = timeoutMs == null ? null : Date.now() + Math.max(0, timeoutMs);
        const readyEvents = [];

        while (readyEvents.length === 0) {
          for (const subscription of subscriptions) {
            // A clock subscription is ready once its deadline has elapsed; report
            // it as a first-class event so it is returned alongside any ready fds
            // (not only as a fallback when nothing else is ready).
            if (subscription.kind === "clock") {
              if (deadline != null && Date.now() >= deadline) {
                readyEvents.push({
                  userdata: subscription.userdata,
                  error: __agentOSWasiErrnoSuccess,
                  type: 0,
                  nbytes: 0,
                  flags: 0,
                });
              }
              continue;
            }
            if (subscription.kind === "fd_read" && subscription.handle?.kind === "pipe-read") {
              const pipe = subscription.handle.pipe;
              if (
                pipe &&
                (pipe.chunks.length > 0 ||
                  (pipe.writeHandleCount === 0 && pipe.producers.size === 0))
              ) {
                readyEvents.push({
                  userdata: subscription.userdata,
                  error: __agentOSWasiErrnoSuccess,
                  type: 1,
                  nbytes: pipe.chunks[0]?.length ?? 0,
                  flags: 0,
                });
              }
              continue;
            }

            // Without a kernel poll bridge, resolve stdin fd_read readiness from
            // the host-seam queued byte count (the browser delivers stdin through
            // the runtime process object). Reporting nbytes does not consume input.
            if (
              !syncRpc &&
              subscription.kind === "fd_read" &&
              subscription.streamKind === "stdin" &&
              typeof __agentOSWasiHost.stdinReadableBytes === "function"
            ) {
              const available = Number(__agentOSWasiHost.stdinReadableBytes()) >>> 0;
              if (available > 0) {
                readyEvents.push({
                  userdata: subscription.userdata,
                  error: __agentOSWasiErrnoSuccess,
                  type: 1,
                  nbytes: available,
                  flags: 0,
                });
              }
              continue;
            }

            if (subscription.kind === "fd_write" && subscription.handle?.kind === "pipe-write") {
              readyEvents.push({
                userdata: subscription.userdata,
                error: __agentOSWasiErrnoSuccess,
                type: 2,
                nbytes: 65536,
                flags: 0,
              });
              continue;
            }

            // Without a kernel poll bridge (a non-native backend) stdout/stderr
            // are always writable, so resolve their fd_write readiness directly
            // instead of leaving it to the (absent) __kernel_poll round-trip.
            if (
              !syncRpc &&
              subscription.kind === "fd_write" &&
              (subscription.streamKind === "stdout" ||
                subscription.streamKind === "stderr")
            ) {
              readyEvents.push({
                userdata: subscription.userdata,
                error: __agentOSWasiErrnoSuccess,
                type: 2,
                nbytes: 65536,
                flags: 0,
              });
            }
          }

          if (readyEvents.length > 0) {
            break;
          }

          // Without a kernel poll bridge, fd readiness is resolved synchronously
          // above (stdio fast paths) or via pipes; if there is no clock to wait on
          // and no pipe to pump, no further progress is possible, so stop instead
          // of busy-waiting until the caller times out.
          if (
            !syncRpc &&
            !subscriptions.some((subscription) => subscription.kind === "clock") &&
            !subscriptions.some(
              (subscription) =>
                subscription.handle?.kind === "pipe-read" ||
                subscription.handle?.kind === "pipe-write",
            )
          ) {
            break;
          }

          const pollTargets = subscriptions
            .filter(
              (subscription) =>
                (subscription.kind === "fd_read" || subscription.kind === "fd_write") &&
                typeof subscription.targetFd === "number",
            )
            .map((subscription) => ({
              fd: subscription.targetFd,
              events: subscription.kind === "fd_read" ? kernelPollIn : kernelPollOut,
            }));
          const waitMs =
            deadline == null ? 10 : Math.max(0, Math.min(10, deadline - Date.now()));

          if (syncRpc && pollTargets.length > 0) {
            let response = null;
            try {
              response = syncRpc.callSync("__kernel_poll", [pollTargets, waitMs]);
            } catch (error) {
              __agentOSWasiDebug(
                \`poll_oneoff __kernel_poll failed: \${
                  error instanceof Error ? error.message : String(error)
                }\`,
              );
            }

            const responseEntries = Array.isArray(response?.fds) ? response.fds : [];
            for (const subscription of subscriptions) {
              if (
                (subscription.kind !== "fd_read" && subscription.kind !== "fd_write") ||
                typeof subscription.targetFd !== "number"
              ) {
                continue;
              }

              const responseEntry = responseEntries.find(
                (entry) => (Number(entry?.fd) >>> 0) === subscription.targetFd,
              );
              const revents = Number(responseEntry?.revents) >>> 0;
              const interested =
                subscription.kind === "fd_read"
                  ? kernelPollIn | kernelPollErr | kernelPollHup
                  : kernelPollOut | kernelPollErr | kernelPollHup;
              if ((revents & interested) === 0) {
                continue;
              }

              readyEvents.push({
                userdata: subscription.userdata,
                error: __agentOSWasiErrnoSuccess,
                type: subscription.kind === "fd_read" ? 1 : 2,
                nbytes: subscription.kind === "fd_read" ? 1 : 65536,
                flags: 0,
              });
            }
          }

          if (readyEvents.length > 0) {
            break;
          }

          let pumped = false;
          for (const subscription of subscriptions) {
            if (subscription.kind === "fd_read" && subscription.handle?.kind === "pipe-read") {
              pumped = this._pumpPipeProducers(subscription.handle.pipe, 10) || pumped;
            }
          }

          if (pumped) {
            continue;
          }

          if (deadline != null && Date.now() >= deadline) {
            break;
          }

          if (
            pollTargets.length === 0 &&
            typeof Atomics?.wait !== "function" &&
            deadline == null
          ) {
            break;
          }

          if (
            typeof Atomics?.wait === "function" &&
            typeof syntheticWaitArray !== "undefined"
          ) {
            Atomics.wait(syntheticWaitArray, 0, 0, waitMs);
          } else if (!syncRpc && pollTargets.length === 0) {
            break;
          }
        }

        if (
          readyEvents.length === 0 &&
          subscriptions.some((subscription) => subscription.kind === "clock")
        ) {
          const clockSubscription = subscriptions.find(
            (subscription) => subscription.kind === "clock",
          );
          readyEvents.push({
            userdata: clockSubscription.userdata,
            error: __agentOSWasiErrnoSuccess,
            type: 0,
            nbytes: 0,
            flags: 0,
          });
        }

        for (let index = 0; index < readyEvents.length; index += 1) {
          const base = (Number(outPtr) >>> 0) + index * eventSize;
          const event = readyEvents[index];
          memory.set(event.userdata, base);
          view.setUint16(base + 8, event.error, true);
          view.setUint8(base + 10, event.type);
          view.setBigUint64(base + 16, BigInt(event.nbytes), true);
          view.setUint16(base + 24, event.flags, true);
        }

        return this._writeUint32(neventsPtr, readyEvents.length);
      } catch (error) {
        __agentOSWasiDebug(
          \`poll_oneoff failed: \${error instanceof Error ? error.message : String(error)}\`,
        );
        return __agentOSWasiErrnoFault;
      }
    }

    _randomGet(bufPtr, bufLen) {
      try {
        const length = Number(bufLen) >>> 0;
        const bytes = Buffer.allocUnsafe(length);
        __agentOSCrypto().randomFillSync(bytes);
        return this._writeBytes(bufPtr, bytes);
      } catch {
        return __agentOSWasiErrnoFault;
      }
    }

    _schedYield() {
      return __agentOSWasiErrnoSuccess;
    }

    _procExit(code) {
      if (this.returnOnExit) {
        const error = new Error(\`wasi exit(\${Number(code) >>> 0})\`);
        error.__agentOSWasiExit = true;
        error.code = Number(code) >>> 0;
        throw error;
      }
      process.exit(Number(code) >>> 0);
    }
  }

  Object.defineProperty(globalThis, "__agentOSWasiModule", {
    configurable: true,
    enumerable: false,
    value: { WASI },
    writable: true,
  });
}

		// Re-export the shared runner WASI class as the browser wasi module.
		module.exports = { WASI: globalThis.__agentOSWasiModule.WASI };
		module.exports.default = { WASI: globalThis.__agentOSWasiModule.WASI };
`;
  }
});

// ../../../agent-os/packages/browser/dist/signals.js
var PROCESS_SIGNAL_NUMBERS, VALID_PROCESS_SIGNALS;
var init_signals = __esm({
  "../../../agent-os/packages/browser/dist/signals.js"() {
    "use strict";
    PROCESS_SIGNAL_NUMBERS = {
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
    VALID_PROCESS_SIGNALS = /* @__PURE__ */ new Set([0, ...Object.values(PROCESS_SIGNAL_NUMBERS)]);
  }
});

// ../../../agent-os/packages/browser/dist/generated/buffer-polyfill.js
var BROWSER_BUFFER_POLYFILL_CODE;
var init_buffer_polyfill = __esm({
  "../../../agent-os/packages/browser/dist/generated/buffer-polyfill.js"() {
    "use strict";
    BROWSER_BUFFER_POLYFILL_CODE = `var process = globalThis.process || {
  env: {},
  nextTick: (fn, ...args) => queueMicrotask(() => fn(...args)),
};
var __getOwnPropNames = Object.getOwnPropertyNames;
var __commonJS = (cb, mod) => function __require() {
  return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
};

// node_modules/.pnpm/base64-js@1.5.1/node_modules/base64-js/index.js
var require_base64_js = __commonJS({
  "node_modules/.pnpm/base64-js@1.5.1/node_modules/base64-js/index.js"(exports2) {
    "use strict";
    exports2.byteLength = byteLength;
    exports2.toByteArray = toByteArray;
    exports2.fromByteArray = fromByteArray;
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

// node_modules/.pnpm/ieee754@1.2.1/node_modules/ieee754/index.js
var require_ieee754 = __commonJS({
  "node_modules/.pnpm/ieee754@1.2.1/node_modules/ieee754/index.js"(exports2) {
    exports2.read = function(buffer2, offset, isLE, mLen, nBytes) {
      var e, m;
      var eLen = nBytes * 8 - mLen - 1;
      var eMax = (1 << eLen) - 1;
      var eBias = eMax >> 1;
      var nBits = -7;
      var i = isLE ? nBytes - 1 : 0;
      var d = isLE ? -1 : 1;
      var s = buffer2[offset + i];
      i += d;
      e = s & (1 << -nBits) - 1;
      s >>= -nBits;
      nBits += eLen;
      for (; nBits > 0; e = e * 256 + buffer2[offset + i], i += d, nBits -= 8) {
      }
      m = e & (1 << -nBits) - 1;
      e >>= -nBits;
      nBits += mLen;
      for (; nBits > 0; m = m * 256 + buffer2[offset + i], i += d, nBits -= 8) {
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
    exports2.write = function(buffer2, value, offset, isLE, mLen, nBytes) {
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
      for (; mLen >= 8; buffer2[offset + i] = m & 255, i += d, m /= 256, mLen -= 8) {
      }
      e = e << mLen | m;
      eLen += mLen;
      for (; eLen > 0; buffer2[offset + i] = e & 255, i += d, e /= 256, eLen -= 8) {
      }
      buffer2[offset + i - d] |= s * 128;
    };
  }
});

// node_modules/.pnpm/buffer@5.7.1/node_modules/buffer/index.js
var require_buffer = __commonJS({
  "node_modules/.pnpm/buffer@5.7.1/node_modules/buffer/index.js"(exports2) {
    "use strict";
    var base64 = require_base64_js();
    var ieee754 = require_ieee754();
    var customInspectSymbol = typeof Symbol === "function" && typeof Symbol["for"] === "function" ? Symbol["for"]("nodejs.util.inspect.custom") : null;
    exports2.Buffer = Buffer2;
    exports2.SlowBuffer = SlowBuffer;
    exports2.INSPECT_MAX_BYTES = 50;
    var K_MAX_LENGTH = 2147483647;
    exports2.kMaxLength = K_MAX_LENGTH;
    Buffer2.TYPED_ARRAY_SUPPORT = typedArraySupport();
    if (!Buffer2.TYPED_ARRAY_SUPPORT && typeof console !== "undefined" && typeof console.error === "function") {
      console.error(
        "This browser lacks typed array (Uint8Array) support which is required by \`buffer\` v5.x. Use \`buffer\` v4.x if you require old browser support."
      );
    }
    function typedArraySupport() {
      try {
        var arr = new Uint8Array(1);
        var proto = { foo: function() {
          return 42;
        } };
        Object.setPrototypeOf(proto, Uint8Array.prototype);
        Object.setPrototypeOf(arr, proto);
        return arr.foo() === 42;
      } catch (e) {
        return false;
      }
    }
    Object.defineProperty(Buffer2.prototype, "parent", {
      enumerable: true,
      get: function() {
        if (!Buffer2.isBuffer(this)) return void 0;
        return this.buffer;
      }
    });
    Object.defineProperty(Buffer2.prototype, "offset", {
      enumerable: true,
      get: function() {
        if (!Buffer2.isBuffer(this)) return void 0;
        return this.byteOffset;
      }
    });
    function createBuffer(length) {
      if (length > K_MAX_LENGTH) {
        throw new RangeError('The value "' + length + '" is invalid for option "size"');
      }
      var buf = new Uint8Array(length);
      Object.setPrototypeOf(buf, Buffer2.prototype);
      return buf;
    }
    function Buffer2(arg, encodingOrOffset, length) {
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
    Buffer2.poolSize = 8192;
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
      var valueOf = value.valueOf && value.valueOf();
      if (valueOf != null && valueOf !== value) {
        return Buffer2.from(valueOf, encodingOrOffset, length);
      }
      var b = fromObject(value);
      if (b) return b;
      if (typeof Symbol !== "undefined" && Symbol.toPrimitive != null && typeof value[Symbol.toPrimitive] === "function") {
        return Buffer2.from(
          value[Symbol.toPrimitive]("string"),
          encodingOrOffset,
          length
        );
      }
      throw new TypeError(
        "The first argument must be one of type string, Buffer, ArrayBuffer, Array, or Array-like Object. Received type " + typeof value
      );
    }
    Buffer2.from = function(value, encodingOrOffset, length) {
      return from(value, encodingOrOffset, length);
    };
    Object.setPrototypeOf(Buffer2.prototype, Uint8Array.prototype);
    Object.setPrototypeOf(Buffer2, Uint8Array);
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
    Buffer2.alloc = function(size, fill, encoding) {
      return alloc(size, fill, encoding);
    };
    function allocUnsafe(size) {
      assertSize(size);
      return createBuffer(size < 0 ? 0 : checked(size) | 0);
    }
    Buffer2.allocUnsafe = function(size) {
      return allocUnsafe(size);
    };
    Buffer2.allocUnsafeSlow = function(size) {
      return allocUnsafe(size);
    };
    function fromString(string, encoding) {
      if (typeof encoding !== "string" || encoding === "") {
        encoding = "utf8";
      }
      if (!Buffer2.isEncoding(encoding)) {
        throw new TypeError("Unknown encoding: " + encoding);
      }
      var length = byteLength(string, encoding) | 0;
      var buf = createBuffer(length);
      var actual = buf.write(string, encoding);
      if (actual !== length) {
        buf = buf.slice(0, actual);
      }
      return buf;
    }
    function fromArrayLike(array) {
      var length = array.length < 0 ? 0 : checked(array.length) | 0;
      var buf = createBuffer(length);
      for (var i = 0; i < length; i += 1) {
        buf[i] = array[i] & 255;
      }
      return buf;
    }
    function fromArrayView(arrayView) {
      if (isInstance(arrayView, Uint8Array)) {
        var copy = new Uint8Array(arrayView);
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
      var buf;
      if (byteOffset === void 0 && length === void 0) {
        buf = new Uint8Array(array);
      } else if (length === void 0) {
        buf = new Uint8Array(array, byteOffset);
      } else {
        buf = new Uint8Array(array, byteOffset, length);
      }
      Object.setPrototypeOf(buf, Buffer2.prototype);
      return buf;
    }
    function fromObject(obj) {
      if (Buffer2.isBuffer(obj)) {
        var len = checked(obj.length) | 0;
        var buf = createBuffer(len);
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
      return Buffer2.alloc(+length);
    }
    Buffer2.isBuffer = function isBuffer(b) {
      return b != null && b._isBuffer === true && b !== Buffer2.prototype;
    };
    Buffer2.compare = function compare(a, b) {
      if (isInstance(a, Uint8Array)) a = Buffer2.from(a, a.offset, a.byteLength);
      if (isInstance(b, Uint8Array)) b = Buffer2.from(b, b.offset, b.byteLength);
      if (!Buffer2.isBuffer(a) || !Buffer2.isBuffer(b)) {
        throw new TypeError(
          'The "buf1", "buf2" arguments must be one of type Buffer or Uint8Array'
        );
      }
      if (a === b) return 0;
      var x = a.length;
      var y = b.length;
      for (var i = 0, len = Math.min(x, y); i < len; ++i) {
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
    Buffer2.isEncoding = function isEncoding(encoding) {
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
    Buffer2.concat = function concat(list, length) {
      if (!Array.isArray(list)) {
        throw new TypeError('"list" argument must be an Array of Buffers');
      }
      if (list.length === 0) {
        return Buffer2.alloc(0);
      }
      var i;
      if (length === void 0) {
        length = 0;
        for (i = 0; i < list.length; ++i) {
          length += list[i].length;
        }
      }
      var buffer2 = Buffer2.allocUnsafe(length);
      var pos = 0;
      for (i = 0; i < list.length; ++i) {
        var buf = list[i];
        if (isInstance(buf, Uint8Array)) {
          if (pos + buf.length > buffer2.length) {
            Buffer2.from(buf).copy(buffer2, pos);
          } else {
            Uint8Array.prototype.set.call(
              buffer2,
              buf,
              pos
            );
          }
        } else if (!Buffer2.isBuffer(buf)) {
          throw new TypeError('"list" argument must be an Array of Buffers');
        } else {
          buf.copy(buffer2, pos);
        }
        pos += buf.length;
      }
      return buffer2;
    };
    function byteLength(string, encoding) {
      if (Buffer2.isBuffer(string)) {
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
      var len = string.length;
      var mustMatch = arguments.length > 2 && arguments[2] === true;
      if (!mustMatch && len === 0) return 0;
      var loweredCase = false;
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
    Buffer2.byteLength = byteLength;
    function slowToString(encoding, start, end) {
      var loweredCase = false;
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
    Buffer2.prototype._isBuffer = true;
    function swap(b, n, m) {
      var i = b[n];
      b[n] = b[m];
      b[m] = i;
    }
    Buffer2.prototype.swap16 = function swap16() {
      var len = this.length;
      if (len % 2 !== 0) {
        throw new RangeError("Buffer size must be a multiple of 16-bits");
      }
      for (var i = 0; i < len; i += 2) {
        swap(this, i, i + 1);
      }
      return this;
    };
    Buffer2.prototype.swap32 = function swap32() {
      var len = this.length;
      if (len % 4 !== 0) {
        throw new RangeError("Buffer size must be a multiple of 32-bits");
      }
      for (var i = 0; i < len; i += 4) {
        swap(this, i, i + 3);
        swap(this, i + 1, i + 2);
      }
      return this;
    };
    Buffer2.prototype.swap64 = function swap64() {
      var len = this.length;
      if (len % 8 !== 0) {
        throw new RangeError("Buffer size must be a multiple of 64-bits");
      }
      for (var i = 0; i < len; i += 8) {
        swap(this, i, i + 7);
        swap(this, i + 1, i + 6);
        swap(this, i + 2, i + 5);
        swap(this, i + 3, i + 4);
      }
      return this;
    };
    Buffer2.prototype.toString = function toString() {
      var length = this.length;
      if (length === 0) return "";
      if (arguments.length === 0) return utf8Slice(this, 0, length);
      return slowToString.apply(this, arguments);
    };
    Buffer2.prototype.toLocaleString = Buffer2.prototype.toString;
    Buffer2.prototype.equals = function equals(b) {
      if (!Buffer2.isBuffer(b)) throw new TypeError("Argument must be a Buffer");
      if (this === b) return true;
      return Buffer2.compare(this, b) === 0;
    };
    Buffer2.prototype.inspect = function inspect() {
      var str = "";
      var max = exports2.INSPECT_MAX_BYTES;
      str = this.toString("hex", 0, max).replace(/(.{2})/g, "$1 ").trim();
      if (this.length > max) str += " ... ";
      return "<Buffer " + str + ">";
    };
    if (customInspectSymbol) {
      Buffer2.prototype[customInspectSymbol] = Buffer2.prototype.inspect;
    }
    Buffer2.prototype.compare = function compare(target, start, end, thisStart, thisEnd) {
      if (isInstance(target, Uint8Array)) {
        target = Buffer2.from(target, target.offset, target.byteLength);
      }
      if (!Buffer2.isBuffer(target)) {
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
      var x = thisEnd - thisStart;
      var y = end - start;
      var len = Math.min(x, y);
      var thisCopy = this.slice(thisStart, thisEnd);
      var targetCopy = target.slice(start, end);
      for (var i = 0; i < len; ++i) {
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
    function bidirectionalIndexOf(buffer2, val, byteOffset, encoding, dir) {
      if (buffer2.length === 0) return -1;
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
        byteOffset = dir ? 0 : buffer2.length - 1;
      }
      if (byteOffset < 0) byteOffset = buffer2.length + byteOffset;
      if (byteOffset >= buffer2.length) {
        if (dir) return -1;
        else byteOffset = buffer2.length - 1;
      } else if (byteOffset < 0) {
        if (dir) byteOffset = 0;
        else return -1;
      }
      if (typeof val === "string") {
        val = Buffer2.from(val, encoding);
      }
      if (Buffer2.isBuffer(val)) {
        if (val.length === 0) {
          return -1;
        }
        return arrayIndexOf(buffer2, val, byteOffset, encoding, dir);
      } else if (typeof val === "number") {
        val = val & 255;
        if (typeof Uint8Array.prototype.indexOf === "function") {
          if (dir) {
            return Uint8Array.prototype.indexOf.call(buffer2, val, byteOffset);
          } else {
            return Uint8Array.prototype.lastIndexOf.call(buffer2, val, byteOffset);
          }
        }
        return arrayIndexOf(buffer2, [val], byteOffset, encoding, dir);
      }
      throw new TypeError("val must be string, number or Buffer");
    }
    function arrayIndexOf(arr, val, byteOffset, encoding, dir) {
      var indexSize = 1;
      var arrLength = arr.length;
      var valLength = val.length;
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
      var i;
      if (dir) {
        var foundIndex = -1;
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
          var found = true;
          for (var j = 0; j < valLength; j++) {
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
    Buffer2.prototype.includes = function includes(val, byteOffset, encoding) {
      return this.indexOf(val, byteOffset, encoding) !== -1;
    };
    Buffer2.prototype.indexOf = function indexOf(val, byteOffset, encoding) {
      return bidirectionalIndexOf(this, val, byteOffset, encoding, true);
    };
    Buffer2.prototype.lastIndexOf = function lastIndexOf(val, byteOffset, encoding) {
      return bidirectionalIndexOf(this, val, byteOffset, encoding, false);
    };
    function hexWrite(buf, string, offset, length) {
      offset = Number(offset) || 0;
      var remaining = buf.length - offset;
      if (!length) {
        length = remaining;
      } else {
        length = Number(length);
        if (length > remaining) {
          length = remaining;
        }
      }
      var strLen = string.length;
      if (length > strLen / 2) {
        length = strLen / 2;
      }
      for (var i = 0; i < length; ++i) {
        var parsed = parseInt(string.substr(i * 2, 2), 16);
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
    Buffer2.prototype.write = function write(string, offset, length, encoding) {
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
      var remaining = this.length - offset;
      if (length === void 0 || length > remaining) length = remaining;
      if (string.length > 0 && (length < 0 || offset < 0) || offset > this.length) {
        throw new RangeError("Attempt to write outside buffer bounds");
      }
      if (!encoding) encoding = "utf8";
      var loweredCase = false;
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
    Buffer2.prototype.toJSON = function toJSON() {
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
      var res = [];
      var i = start;
      while (i < end) {
        var firstByte = buf[i];
        var codePoint = null;
        var bytesPerSequence = firstByte > 239 ? 4 : firstByte > 223 ? 3 : firstByte > 191 ? 2 : 1;
        if (i + bytesPerSequence <= end) {
          var secondByte, thirdByte, fourthByte, tempCodePoint;
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
      var len = codePoints.length;
      if (len <= MAX_ARGUMENTS_LENGTH) {
        return String.fromCharCode.apply(String, codePoints);
      }
      var res = "";
      var i = 0;
      while (i < len) {
        res += String.fromCharCode.apply(
          String,
          codePoints.slice(i, i += MAX_ARGUMENTS_LENGTH)
        );
      }
      return res;
    }
    function asciiSlice(buf, start, end) {
      var ret = "";
      end = Math.min(buf.length, end);
      for (var i = start; i < end; ++i) {
        ret += String.fromCharCode(buf[i] & 127);
      }
      return ret;
    }
    function latin1Slice(buf, start, end) {
      var ret = "";
      end = Math.min(buf.length, end);
      for (var i = start; i < end; ++i) {
        ret += String.fromCharCode(buf[i]);
      }
      return ret;
    }
    function hexSlice(buf, start, end) {
      var len = buf.length;
      if (!start || start < 0) start = 0;
      if (!end || end < 0 || end > len) end = len;
      var out = "";
      for (var i = start; i < end; ++i) {
        out += hexSliceLookupTable[buf[i]];
      }
      return out;
    }
    function utf16leSlice(buf, start, end) {
      var bytes = buf.slice(start, end);
      var res = "";
      for (var i = 0; i < bytes.length - 1; i += 2) {
        res += String.fromCharCode(bytes[i] + bytes[i + 1] * 256);
      }
      return res;
    }
    Buffer2.prototype.slice = function slice(start, end) {
      var len = this.length;
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
      var newBuf = this.subarray(start, end);
      Object.setPrototypeOf(newBuf, Buffer2.prototype);
      return newBuf;
    };
    function checkOffset(offset, ext, length) {
      if (offset % 1 !== 0 || offset < 0) throw new RangeError("offset is not uint");
      if (offset + ext > length) throw new RangeError("Trying to access beyond buffer length");
    }
    Buffer2.prototype.readUintLE = Buffer2.prototype.readUIntLE = function readUIntLE(offset, byteLength2, noAssert) {
      offset = offset >>> 0;
      byteLength2 = byteLength2 >>> 0;
      if (!noAssert) checkOffset(offset, byteLength2, this.length);
      var val = this[offset];
      var mul = 1;
      var i = 0;
      while (++i < byteLength2 && (mul *= 256)) {
        val += this[offset + i] * mul;
      }
      return val;
    };
    Buffer2.prototype.readUintBE = Buffer2.prototype.readUIntBE = function readUIntBE(offset, byteLength2, noAssert) {
      offset = offset >>> 0;
      byteLength2 = byteLength2 >>> 0;
      if (!noAssert) {
        checkOffset(offset, byteLength2, this.length);
      }
      var val = this[offset + --byteLength2];
      var mul = 1;
      while (byteLength2 > 0 && (mul *= 256)) {
        val += this[offset + --byteLength2] * mul;
      }
      return val;
    };
    Buffer2.prototype.readUint8 = Buffer2.prototype.readUInt8 = function readUInt8(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 1, this.length);
      return this[offset];
    };
    Buffer2.prototype.readUint16LE = Buffer2.prototype.readUInt16LE = function readUInt16LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      return this[offset] | this[offset + 1] << 8;
    };
    Buffer2.prototype.readUint16BE = Buffer2.prototype.readUInt16BE = function readUInt16BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      return this[offset] << 8 | this[offset + 1];
    };
    Buffer2.prototype.readUint32LE = Buffer2.prototype.readUInt32LE = function readUInt32LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return (this[offset] | this[offset + 1] << 8 | this[offset + 2] << 16) + this[offset + 3] * 16777216;
    };
    Buffer2.prototype.readUint32BE = Buffer2.prototype.readUInt32BE = function readUInt32BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return this[offset] * 16777216 + (this[offset + 1] << 16 | this[offset + 2] << 8 | this[offset + 3]);
    };
    Buffer2.prototype.readIntLE = function readIntLE(offset, byteLength2, noAssert) {
      offset = offset >>> 0;
      byteLength2 = byteLength2 >>> 0;
      if (!noAssert) checkOffset(offset, byteLength2, this.length);
      var val = this[offset];
      var mul = 1;
      var i = 0;
      while (++i < byteLength2 && (mul *= 256)) {
        val += this[offset + i] * mul;
      }
      mul *= 128;
      if (val >= mul) val -= Math.pow(2, 8 * byteLength2);
      return val;
    };
    Buffer2.prototype.readIntBE = function readIntBE(offset, byteLength2, noAssert) {
      offset = offset >>> 0;
      byteLength2 = byteLength2 >>> 0;
      if (!noAssert) checkOffset(offset, byteLength2, this.length);
      var i = byteLength2;
      var mul = 1;
      var val = this[offset + --i];
      while (i > 0 && (mul *= 256)) {
        val += this[offset + --i] * mul;
      }
      mul *= 128;
      if (val >= mul) val -= Math.pow(2, 8 * byteLength2);
      return val;
    };
    Buffer2.prototype.readInt8 = function readInt8(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 1, this.length);
      if (!(this[offset] & 128)) return this[offset];
      return (255 - this[offset] + 1) * -1;
    };
    Buffer2.prototype.readInt16LE = function readInt16LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      var val = this[offset] | this[offset + 1] << 8;
      return val & 32768 ? val | 4294901760 : val;
    };
    Buffer2.prototype.readInt16BE = function readInt16BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 2, this.length);
      var val = this[offset + 1] | this[offset] << 8;
      return val & 32768 ? val | 4294901760 : val;
    };
    Buffer2.prototype.readInt32LE = function readInt32LE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return this[offset] | this[offset + 1] << 8 | this[offset + 2] << 16 | this[offset + 3] << 24;
    };
    Buffer2.prototype.readInt32BE = function readInt32BE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return this[offset] << 24 | this[offset + 1] << 16 | this[offset + 2] << 8 | this[offset + 3];
    };
    Buffer2.prototype.readFloatLE = function readFloatLE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return ieee754.read(this, offset, true, 23, 4);
    };
    Buffer2.prototype.readFloatBE = function readFloatBE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 4, this.length);
      return ieee754.read(this, offset, false, 23, 4);
    };
    Buffer2.prototype.readDoubleLE = function readDoubleLE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 8, this.length);
      return ieee754.read(this, offset, true, 52, 8);
    };
    Buffer2.prototype.readDoubleBE = function readDoubleBE(offset, noAssert) {
      offset = offset >>> 0;
      if (!noAssert) checkOffset(offset, 8, this.length);
      return ieee754.read(this, offset, false, 52, 8);
    };
    function checkInt(buf, value, offset, ext, max, min) {
      if (!Buffer2.isBuffer(buf)) throw new TypeError('"buffer" argument must be a Buffer instance');
      if (value > max || value < min) throw new RangeError('"value" argument is out of bounds');
      if (offset + ext > buf.length) throw new RangeError("Index out of range");
    }
    Buffer2.prototype.writeUintLE = Buffer2.prototype.writeUIntLE = function writeUIntLE(value, offset, byteLength2, noAssert) {
      value = +value;
      offset = offset >>> 0;
      byteLength2 = byteLength2 >>> 0;
      if (!noAssert) {
        var maxBytes = Math.pow(2, 8 * byteLength2) - 1;
        checkInt(this, value, offset, byteLength2, maxBytes, 0);
      }
      var mul = 1;
      var i = 0;
      this[offset] = value & 255;
      while (++i < byteLength2 && (mul *= 256)) {
        this[offset + i] = value / mul & 255;
      }
      return offset + byteLength2;
    };
    Buffer2.prototype.writeUintBE = Buffer2.prototype.writeUIntBE = function writeUIntBE(value, offset, byteLength2, noAssert) {
      value = +value;
      offset = offset >>> 0;
      byteLength2 = byteLength2 >>> 0;
      if (!noAssert) {
        var maxBytes = Math.pow(2, 8 * byteLength2) - 1;
        checkInt(this, value, offset, byteLength2, maxBytes, 0);
      }
      var i = byteLength2 - 1;
      var mul = 1;
      this[offset + i] = value & 255;
      while (--i >= 0 && (mul *= 256)) {
        this[offset + i] = value / mul & 255;
      }
      return offset + byteLength2;
    };
    Buffer2.prototype.writeUint8 = Buffer2.prototype.writeUInt8 = function writeUInt8(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 1, 255, 0);
      this[offset] = value & 255;
      return offset + 1;
    };
    Buffer2.prototype.writeUint16LE = Buffer2.prototype.writeUInt16LE = function writeUInt16LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 65535, 0);
      this[offset] = value & 255;
      this[offset + 1] = value >>> 8;
      return offset + 2;
    };
    Buffer2.prototype.writeUint16BE = Buffer2.prototype.writeUInt16BE = function writeUInt16BE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 65535, 0);
      this[offset] = value >>> 8;
      this[offset + 1] = value & 255;
      return offset + 2;
    };
    Buffer2.prototype.writeUint32LE = Buffer2.prototype.writeUInt32LE = function writeUInt32LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 4, 4294967295, 0);
      this[offset + 3] = value >>> 24;
      this[offset + 2] = value >>> 16;
      this[offset + 1] = value >>> 8;
      this[offset] = value & 255;
      return offset + 4;
    };
    Buffer2.prototype.writeUint32BE = Buffer2.prototype.writeUInt32BE = function writeUInt32BE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 4, 4294967295, 0);
      this[offset] = value >>> 24;
      this[offset + 1] = value >>> 16;
      this[offset + 2] = value >>> 8;
      this[offset + 3] = value & 255;
      return offset + 4;
    };
    Buffer2.prototype.writeIntLE = function writeIntLE(value, offset, byteLength2, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) {
        var limit = Math.pow(2, 8 * byteLength2 - 1);
        checkInt(this, value, offset, byteLength2, limit - 1, -limit);
      }
      var i = 0;
      var mul = 1;
      var sub = 0;
      this[offset] = value & 255;
      while (++i < byteLength2 && (mul *= 256)) {
        if (value < 0 && sub === 0 && this[offset + i - 1] !== 0) {
          sub = 1;
        }
        this[offset + i] = (value / mul >> 0) - sub & 255;
      }
      return offset + byteLength2;
    };
    Buffer2.prototype.writeIntBE = function writeIntBE(value, offset, byteLength2, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) {
        var limit = Math.pow(2, 8 * byteLength2 - 1);
        checkInt(this, value, offset, byteLength2, limit - 1, -limit);
      }
      var i = byteLength2 - 1;
      var mul = 1;
      var sub = 0;
      this[offset + i] = value & 255;
      while (--i >= 0 && (mul *= 256)) {
        if (value < 0 && sub === 0 && this[offset + i + 1] !== 0) {
          sub = 1;
        }
        this[offset + i] = (value / mul >> 0) - sub & 255;
      }
      return offset + byteLength2;
    };
    Buffer2.prototype.writeInt8 = function writeInt8(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 1, 127, -128);
      if (value < 0) value = 255 + value + 1;
      this[offset] = value & 255;
      return offset + 1;
    };
    Buffer2.prototype.writeInt16LE = function writeInt16LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 32767, -32768);
      this[offset] = value & 255;
      this[offset + 1] = value >>> 8;
      return offset + 2;
    };
    Buffer2.prototype.writeInt16BE = function writeInt16BE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 2, 32767, -32768);
      this[offset] = value >>> 8;
      this[offset + 1] = value & 255;
      return offset + 2;
    };
    Buffer2.prototype.writeInt32LE = function writeInt32LE(value, offset, noAssert) {
      value = +value;
      offset = offset >>> 0;
      if (!noAssert) checkInt(this, value, offset, 4, 2147483647, -2147483648);
      this[offset] = value & 255;
      this[offset + 1] = value >>> 8;
      this[offset + 2] = value >>> 16;
      this[offset + 3] = value >>> 24;
      return offset + 4;
    };
    Buffer2.prototype.writeInt32BE = function writeInt32BE(value, offset, noAssert) {
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
    Buffer2.prototype.writeFloatLE = function writeFloatLE(value, offset, noAssert) {
      return writeFloat(this, value, offset, true, noAssert);
    };
    Buffer2.prototype.writeFloatBE = function writeFloatBE(value, offset, noAssert) {
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
    Buffer2.prototype.writeDoubleLE = function writeDoubleLE(value, offset, noAssert) {
      return writeDouble(this, value, offset, true, noAssert);
    };
    Buffer2.prototype.writeDoubleBE = function writeDoubleBE(value, offset, noAssert) {
      return writeDouble(this, value, offset, false, noAssert);
    };
    Buffer2.prototype.copy = function copy(target, targetStart, start, end) {
      if (!Buffer2.isBuffer(target)) throw new TypeError("argument should be a Buffer");
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
      var len = end - start;
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
    Buffer2.prototype.fill = function fill(val, start, end, encoding) {
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
        if (typeof encoding === "string" && !Buffer2.isEncoding(encoding)) {
          throw new TypeError("Unknown encoding: " + encoding);
        }
        if (val.length === 1) {
          var code = val.charCodeAt(0);
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
      var i;
      if (typeof val === "number") {
        for (i = start; i < end; ++i) {
          this[i] = val;
        }
      } else {
        var bytes = Buffer2.isBuffer(val) ? val : Buffer2.from(val, encoding);
        var len = bytes.length;
        if (len === 0) {
          throw new TypeError('The value "' + val + '" is invalid for argument "value"');
        }
        for (i = 0; i < end - start; ++i) {
          this[i + start] = bytes[i % len];
        }
      }
      return this;
    };
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
      var codePoint;
      var length = string.length;
      var leadSurrogate = null;
      var bytes = [];
      for (var i = 0; i < length; ++i) {
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
      var byteArray = [];
      for (var i = 0; i < str.length; ++i) {
        byteArray.push(str.charCodeAt(i) & 255);
      }
      return byteArray;
    }
    function utf16leToBytes(str, units) {
      var c, hi, lo;
      var byteArray = [];
      for (var i = 0; i < str.length; ++i) {
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
      for (var i = 0; i < length; ++i) {
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
      var alphabet = "0123456789abcdef";
      var table = new Array(256);
      for (var i = 0; i < 16; ++i) {
        var i16 = i * 16;
        for (var j = 0; j < 16; ++j) {
          table[i16 + j] = alphabet[i] + alphabet[j];
        }
      }
      return table;
    })();
  }
});

// <stdin>
var buffer = require_buffer();
module.exports = buffer.default ?? buffer;
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

if (module.exports && module.exports.default == null) module.exports.default = module.exports;
`;
  }
});

// ../../../agent-os/packages/browser/dist/generated/path-polyfill.js
var BROWSER_PATH_POLYFILL_CODE;
var init_path_polyfill = __esm({
  "../../../agent-os/packages/browser/dist/generated/path-polyfill.js"() {
    "use strict";
    BROWSER_PATH_POLYFILL_CODE = `var process = globalThis.process || {
  env: {},
  cwd: () => '/',
};
var __getOwnPropNames = Object.getOwnPropertyNames;
var __commonJS = (cb, mod) => function __require() {
  return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
};

// node_modules/.pnpm/path-browserify@1.0.1/node_modules/path-browserify/index.js
var require_path_browserify = __commonJS({
  "node_modules/.pnpm/path-browserify@1.0.1/node_modules/path-browserify/index.js"(exports2, module2) {
    "use strict";
    function assertPath(path2) {
      if (typeof path2 !== "string") {
        throw new TypeError("Path must be a string. Received " + JSON.stringify(path2));
      }
    }
    function normalizeStringPosix(path2, allowAboveRoot) {
      var res = "";
      var lastSegmentLength = 0;
      var lastSlash = -1;
      var dots = 0;
      var code;
      for (var i = 0; i <= path2.length; ++i) {
        if (i < path2.length)
          code = path2.charCodeAt(i);
        else if (code === 47)
          break;
        else
          code = 47;
        if (code === 47) {
          if (lastSlash === i - 1 || dots === 1) {
          } else if (lastSlash !== i - 1 && dots === 2) {
            if (res.length < 2 || lastSegmentLength !== 2 || res.charCodeAt(res.length - 1) !== 46 || res.charCodeAt(res.length - 2) !== 46) {
              if (res.length > 2) {
                var lastSlashIndex = res.lastIndexOf("/");
                if (lastSlashIndex !== res.length - 1) {
                  if (lastSlashIndex === -1) {
                    res = "";
                    lastSegmentLength = 0;
                  } else {
                    res = res.slice(0, lastSlashIndex);
                    lastSegmentLength = res.length - 1 - res.lastIndexOf("/");
                  }
                  lastSlash = i;
                  dots = 0;
                  continue;
                }
              } else if (res.length === 2 || res.length === 1) {
                res = "";
                lastSegmentLength = 0;
                lastSlash = i;
                dots = 0;
                continue;
              }
            }
            if (allowAboveRoot) {
              if (res.length > 0)
                res += "/..";
              else
                res = "..";
              lastSegmentLength = 2;
            }
          } else {
            if (res.length > 0)
              res += "/" + path2.slice(lastSlash + 1, i);
            else
              res = path2.slice(lastSlash + 1, i);
            lastSegmentLength = i - lastSlash - 1;
          }
          lastSlash = i;
          dots = 0;
        } else if (code === 46 && dots !== -1) {
          ++dots;
        } else {
          dots = -1;
        }
      }
      return res;
    }
    function _format(sep, pathObject) {
      var dir = pathObject.dir || pathObject.root;
      var base = pathObject.base || (pathObject.name || "") + (pathObject.ext || "");
      if (!dir) {
        return base;
      }
      if (dir === pathObject.root) {
        return dir + base;
      }
      return dir + sep + base;
    }
    var posix2 = {
      // path.resolve([from ...], to)
      resolve: function resolve() {
        var resolvedPath = "";
        var resolvedAbsolute = false;
        var cwd;
        for (var i = arguments.length - 1; i >= -1 && !resolvedAbsolute; i--) {
          var path2;
          if (i >= 0)
            path2 = arguments[i];
          else {
            if (cwd === void 0)
              cwd = process.cwd();
            path2 = cwd;
          }
          assertPath(path2);
          if (path2.length === 0) {
            continue;
          }
          resolvedPath = path2 + "/" + resolvedPath;
          resolvedAbsolute = path2.charCodeAt(0) === 47;
        }
        resolvedPath = normalizeStringPosix(resolvedPath, !resolvedAbsolute);
        if (resolvedAbsolute) {
          if (resolvedPath.length > 0)
            return "/" + resolvedPath;
          else
            return "/";
        } else if (resolvedPath.length > 0) {
          return resolvedPath;
        } else {
          return ".";
        }
      },
      normalize: function normalize(path2) {
        assertPath(path2);
        if (path2.length === 0) return ".";
        var isAbsolute = path2.charCodeAt(0) === 47;
        var trailingSeparator = path2.charCodeAt(path2.length - 1) === 47;
        path2 = normalizeStringPosix(path2, !isAbsolute);
        if (path2.length === 0 && !isAbsolute) path2 = ".";
        if (path2.length > 0 && trailingSeparator) path2 += "/";
        if (isAbsolute) return "/" + path2;
        return path2;
      },
      isAbsolute: function isAbsolute(path2) {
        assertPath(path2);
        return path2.length > 0 && path2.charCodeAt(0) === 47;
      },
      join: function join() {
        if (arguments.length === 0)
          return ".";
        var joined;
        for (var i = 0; i < arguments.length; ++i) {
          var arg = arguments[i];
          assertPath(arg);
          if (arg.length > 0) {
            if (joined === void 0)
              joined = arg;
            else
              joined += "/" + arg;
          }
        }
        if (joined === void 0)
          return ".";
        return posix2.normalize(joined);
      },
      relative: function relative(from, to) {
        assertPath(from);
        assertPath(to);
        if (from === to) return "";
        from = posix2.resolve(from);
        to = posix2.resolve(to);
        if (from === to) return "";
        var fromStart = 1;
        for (; fromStart < from.length; ++fromStart) {
          if (from.charCodeAt(fromStart) !== 47)
            break;
        }
        var fromEnd = from.length;
        var fromLen = fromEnd - fromStart;
        var toStart = 1;
        for (; toStart < to.length; ++toStart) {
          if (to.charCodeAt(toStart) !== 47)
            break;
        }
        var toEnd = to.length;
        var toLen = toEnd - toStart;
        var length = fromLen < toLen ? fromLen : toLen;
        var lastCommonSep = -1;
        var i = 0;
        for (; i <= length; ++i) {
          if (i === length) {
            if (toLen > length) {
              if (to.charCodeAt(toStart + i) === 47) {
                return to.slice(toStart + i + 1);
              } else if (i === 0) {
                return to.slice(toStart + i);
              }
            } else if (fromLen > length) {
              if (from.charCodeAt(fromStart + i) === 47) {
                lastCommonSep = i;
              } else if (i === 0) {
                lastCommonSep = 0;
              }
            }
            break;
          }
          var fromCode = from.charCodeAt(fromStart + i);
          var toCode = to.charCodeAt(toStart + i);
          if (fromCode !== toCode)
            break;
          else if (fromCode === 47)
            lastCommonSep = i;
        }
        var out = "";
        for (i = fromStart + lastCommonSep + 1; i <= fromEnd; ++i) {
          if (i === fromEnd || from.charCodeAt(i) === 47) {
            if (out.length === 0)
              out += "..";
            else
              out += "/..";
          }
        }
        if (out.length > 0)
          return out + to.slice(toStart + lastCommonSep);
        else {
          toStart += lastCommonSep;
          if (to.charCodeAt(toStart) === 47)
            ++toStart;
          return to.slice(toStart);
        }
      },
      _makeLong: function _makeLong(path2) {
        return path2;
      },
      dirname: function dirname(path2) {
        assertPath(path2);
        if (path2.length === 0) return ".";
        var code = path2.charCodeAt(0);
        var hasRoot = code === 47;
        var end = -1;
        var matchedSlash = true;
        for (var i = path2.length - 1; i >= 1; --i) {
          code = path2.charCodeAt(i);
          if (code === 47) {
            if (!matchedSlash) {
              end = i;
              break;
            }
          } else {
            matchedSlash = false;
          }
        }
        if (end === -1) return hasRoot ? "/" : ".";
        if (hasRoot && end === 1) return "//";
        return path2.slice(0, end);
      },
      basename: function basename(path2, ext) {
        if (ext !== void 0 && typeof ext !== "string") throw new TypeError('"ext" argument must be a string');
        assertPath(path2);
        var start = 0;
        var end = -1;
        var matchedSlash = true;
        var i;
        if (ext !== void 0 && ext.length > 0 && ext.length <= path2.length) {
          if (ext.length === path2.length && ext === path2) return "";
          var extIdx = ext.length - 1;
          var firstNonSlashEnd = -1;
          for (i = path2.length - 1; i >= 0; --i) {
            var code = path2.charCodeAt(i);
            if (code === 47) {
              if (!matchedSlash) {
                start = i + 1;
                break;
              }
            } else {
              if (firstNonSlashEnd === -1) {
                matchedSlash = false;
                firstNonSlashEnd = i + 1;
              }
              if (extIdx >= 0) {
                if (code === ext.charCodeAt(extIdx)) {
                  if (--extIdx === -1) {
                    end = i;
                  }
                } else {
                  extIdx = -1;
                  end = firstNonSlashEnd;
                }
              }
            }
          }
          if (start === end) end = firstNonSlashEnd;
          else if (end === -1) end = path2.length;
          return path2.slice(start, end);
        } else {
          for (i = path2.length - 1; i >= 0; --i) {
            if (path2.charCodeAt(i) === 47) {
              if (!matchedSlash) {
                start = i + 1;
                break;
              }
            } else if (end === -1) {
              matchedSlash = false;
              end = i + 1;
            }
          }
          if (end === -1) return "";
          return path2.slice(start, end);
        }
      },
      extname: function extname(path2) {
        assertPath(path2);
        var startDot = -1;
        var startPart = 0;
        var end = -1;
        var matchedSlash = true;
        var preDotState = 0;
        for (var i = path2.length - 1; i >= 0; --i) {
          var code = path2.charCodeAt(i);
          if (code === 47) {
            if (!matchedSlash) {
              startPart = i + 1;
              break;
            }
            continue;
          }
          if (end === -1) {
            matchedSlash = false;
            end = i + 1;
          }
          if (code === 46) {
            if (startDot === -1)
              startDot = i;
            else if (preDotState !== 1)
              preDotState = 1;
          } else if (startDot !== -1) {
            preDotState = -1;
          }
        }
        if (startDot === -1 || end === -1 || // We saw a non-dot character immediately before the dot
        preDotState === 0 || // The (right-most) trimmed path component is exactly '..'
        preDotState === 1 && startDot === end - 1 && startDot === startPart + 1) {
          return "";
        }
        return path2.slice(startDot, end);
      },
      format: function format(pathObject) {
        if (pathObject === null || typeof pathObject !== "object") {
          throw new TypeError('The "pathObject" argument must be of type Object. Received type ' + typeof pathObject);
        }
        return _format("/", pathObject);
      },
      parse: function parse(path2) {
        assertPath(path2);
        var ret = { root: "", dir: "", base: "", ext: "", name: "" };
        if (path2.length === 0) return ret;
        var code = path2.charCodeAt(0);
        var isAbsolute = code === 47;
        var start;
        if (isAbsolute) {
          ret.root = "/";
          start = 1;
        } else {
          start = 0;
        }
        var startDot = -1;
        var startPart = 0;
        var end = -1;
        var matchedSlash = true;
        var i = path2.length - 1;
        var preDotState = 0;
        for (; i >= start; --i) {
          code = path2.charCodeAt(i);
          if (code === 47) {
            if (!matchedSlash) {
              startPart = i + 1;
              break;
            }
            continue;
          }
          if (end === -1) {
            matchedSlash = false;
            end = i + 1;
          }
          if (code === 46) {
            if (startDot === -1) startDot = i;
            else if (preDotState !== 1) preDotState = 1;
          } else if (startDot !== -1) {
            preDotState = -1;
          }
        }
        if (startDot === -1 || end === -1 || // We saw a non-dot character immediately before the dot
        preDotState === 0 || // The (right-most) trimmed path component is exactly '..'
        preDotState === 1 && startDot === end - 1 && startDot === startPart + 1) {
          if (end !== -1) {
            if (startPart === 0 && isAbsolute) ret.base = ret.name = path2.slice(1, end);
            else ret.base = ret.name = path2.slice(startPart, end);
          }
        } else {
          if (startPart === 0 && isAbsolute) {
            ret.name = path2.slice(1, startDot);
            ret.base = path2.slice(1, end);
          } else {
            ret.name = path2.slice(startPart, startDot);
            ret.base = path2.slice(startPart, end);
          }
          ret.ext = path2.slice(startDot, end);
        }
        if (startPart > 0) ret.dir = path2.slice(0, startPart - 1);
        else if (isAbsolute) ret.dir = "/";
        return ret;
      },
      sep: "/",
      delimiter: ":",
      win32: null,
      posix: null
    };
    posix2.posix = posix2;
    module2.exports = posix2;
  }
});

// <stdin>
var path = require_path_browserify();
var resolved = path.default ?? path;
var posix = resolved.posix ?? resolved;
posix.posix = posix;
module.exports = posix;

if (module.exports && module.exports.default == null) module.exports.default = module.exports;
`;
  }
});

// ../../../agent-os/packages/browser/dist/generated/util-polyfill.js
var BROWSER_UTIL_POLYFILL_CODE;
var init_util_polyfill = __esm({
  "../../../agent-os/packages/browser/dist/generated/util-polyfill.js"() {
    "use strict";
    BROWSER_UTIL_POLYFILL_CODE = `var process = globalThis.process || {
  env: {},
  nextTick: (fn, ...args) => queueMicrotask(() => fn(...args)),
};
var __getOwnPropNames = Object.getOwnPropertyNames;
var __commonJS = (cb, mod) => function __require() {
  return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
};

// node_modules/.pnpm/has-symbols@1.1.0/node_modules/has-symbols/shams.js
var require_shams = __commonJS({
  "node_modules/.pnpm/has-symbols@1.1.0/node_modules/has-symbols/shams.js"(exports2, module2) {
    "use strict";
    module2.exports = function hasSymbols() {
      if (typeof Symbol !== "function" || typeof Object.getOwnPropertySymbols !== "function") {
        return false;
      }
      if (typeof Symbol.iterator === "symbol") {
        return true;
      }
      var obj = {};
      var sym = /* @__PURE__ */ Symbol("test");
      var symObj = Object(sym);
      if (typeof sym === "string") {
        return false;
      }
      if (Object.prototype.toString.call(sym) !== "[object Symbol]") {
        return false;
      }
      if (Object.prototype.toString.call(symObj) !== "[object Symbol]") {
        return false;
      }
      var symVal = 42;
      obj[sym] = symVal;
      for (var _ in obj) {
        return false;
      }
      if (typeof Object.keys === "function" && Object.keys(obj).length !== 0) {
        return false;
      }
      if (typeof Object.getOwnPropertyNames === "function" && Object.getOwnPropertyNames(obj).length !== 0) {
        return false;
      }
      var syms = Object.getOwnPropertySymbols(obj);
      if (syms.length !== 1 || syms[0] !== sym) {
        return false;
      }
      if (!Object.prototype.propertyIsEnumerable.call(obj, sym)) {
        return false;
      }
      if (typeof Object.getOwnPropertyDescriptor === "function") {
        var descriptor = (
          /** @type {PropertyDescriptor} */
          Object.getOwnPropertyDescriptor(obj, sym)
        );
        if (descriptor.value !== symVal || descriptor.enumerable !== true) {
          return false;
        }
      }
      return true;
    };
  }
});

// node_modules/.pnpm/has-tostringtag@1.0.2/node_modules/has-tostringtag/shams.js
var require_shams2 = __commonJS({
  "node_modules/.pnpm/has-tostringtag@1.0.2/node_modules/has-tostringtag/shams.js"(exports2, module2) {
    "use strict";
    var hasSymbols = require_shams();
    module2.exports = function hasToStringTagShams() {
      return hasSymbols() && !!Symbol.toStringTag;
    };
  }
});

// node_modules/.pnpm/es-object-atoms@1.1.1/node_modules/es-object-atoms/index.js
var require_es_object_atoms = __commonJS({
  "node_modules/.pnpm/es-object-atoms@1.1.1/node_modules/es-object-atoms/index.js"(exports2, module2) {
    "use strict";
    module2.exports = Object;
  }
});

// node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/index.js
var require_es_errors = __commonJS({
  "node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/index.js"(exports2, module2) {
    "use strict";
    module2.exports = Error;
  }
});

// node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/eval.js
var require_eval = __commonJS({
  "node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/eval.js"(exports2, module2) {
    "use strict";
    module2.exports = EvalError;
  }
});

// node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/range.js
var require_range = __commonJS({
  "node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/range.js"(exports2, module2) {
    "use strict";
    module2.exports = RangeError;
  }
});

// node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/ref.js
var require_ref = __commonJS({
  "node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/ref.js"(exports2, module2) {
    "use strict";
    module2.exports = ReferenceError;
  }
});

// node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/syntax.js
var require_syntax = __commonJS({
  "node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/syntax.js"(exports2, module2) {
    "use strict";
    module2.exports = SyntaxError;
  }
});

// node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/type.js
var require_type = __commonJS({
  "node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/type.js"(exports2, module2) {
    "use strict";
    module2.exports = TypeError;
  }
});

// node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/uri.js
var require_uri = __commonJS({
  "node_modules/.pnpm/es-errors@1.3.0/node_modules/es-errors/uri.js"(exports2, module2) {
    "use strict";
    module2.exports = URIError;
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/abs.js
var require_abs = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/abs.js"(exports2, module2) {
    "use strict";
    module2.exports = Math.abs;
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/floor.js
var require_floor = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/floor.js"(exports2, module2) {
    "use strict";
    module2.exports = Math.floor;
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/max.js
var require_max = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/max.js"(exports2, module2) {
    "use strict";
    module2.exports = Math.max;
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/min.js
var require_min = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/min.js"(exports2, module2) {
    "use strict";
    module2.exports = Math.min;
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/pow.js
var require_pow = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/pow.js"(exports2, module2) {
    "use strict";
    module2.exports = Math.pow;
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/round.js
var require_round = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/round.js"(exports2, module2) {
    "use strict";
    module2.exports = Math.round;
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/isNaN.js
var require_isNaN = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/isNaN.js"(exports2, module2) {
    "use strict";
    module2.exports = Number.isNaN || function isNaN2(a) {
      return a !== a;
    };
  }
});

// node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/sign.js
var require_sign = __commonJS({
  "node_modules/.pnpm/math-intrinsics@1.1.0/node_modules/math-intrinsics/sign.js"(exports2, module2) {
    "use strict";
    var $isNaN = require_isNaN();
    module2.exports = function sign(number) {
      if ($isNaN(number) || number === 0) {
        return number;
      }
      return number < 0 ? -1 : 1;
    };
  }
});

// node_modules/.pnpm/gopd@1.2.0/node_modules/gopd/gOPD.js
var require_gOPD = __commonJS({
  "node_modules/.pnpm/gopd@1.2.0/node_modules/gopd/gOPD.js"(exports2, module2) {
    "use strict";
    module2.exports = Object.getOwnPropertyDescriptor;
  }
});

// node_modules/.pnpm/gopd@1.2.0/node_modules/gopd/index.js
var require_gopd = __commonJS({
  "node_modules/.pnpm/gopd@1.2.0/node_modules/gopd/index.js"(exports2, module2) {
    "use strict";
    var $gOPD = require_gOPD();
    if ($gOPD) {
      try {
        $gOPD([], "length");
      } catch (e) {
        $gOPD = null;
      }
    }
    module2.exports = $gOPD;
  }
});

// node_modules/.pnpm/es-define-property@1.0.1/node_modules/es-define-property/index.js
var require_es_define_property = __commonJS({
  "node_modules/.pnpm/es-define-property@1.0.1/node_modules/es-define-property/index.js"(exports2, module2) {
    "use strict";
    var $defineProperty = Object.defineProperty || false;
    if ($defineProperty) {
      try {
        $defineProperty({}, "a", { value: 1 });
      } catch (e) {
        $defineProperty = false;
      }
    }
    module2.exports = $defineProperty;
  }
});

// node_modules/.pnpm/has-symbols@1.1.0/node_modules/has-symbols/index.js
var require_has_symbols = __commonJS({
  "node_modules/.pnpm/has-symbols@1.1.0/node_modules/has-symbols/index.js"(exports2, module2) {
    "use strict";
    var origSymbol = typeof Symbol !== "undefined" && Symbol;
    var hasSymbolSham = require_shams();
    module2.exports = function hasNativeSymbols() {
      if (typeof origSymbol !== "function") {
        return false;
      }
      if (typeof Symbol !== "function") {
        return false;
      }
      if (typeof origSymbol("foo") !== "symbol") {
        return false;
      }
      if (typeof /* @__PURE__ */ Symbol("bar") !== "symbol") {
        return false;
      }
      return hasSymbolSham();
    };
  }
});

// node_modules/.pnpm/get-proto@1.0.1/node_modules/get-proto/Reflect.getPrototypeOf.js
var require_Reflect_getPrototypeOf = __commonJS({
  "node_modules/.pnpm/get-proto@1.0.1/node_modules/get-proto/Reflect.getPrototypeOf.js"(exports2, module2) {
    "use strict";
    module2.exports = typeof Reflect !== "undefined" && Reflect.getPrototypeOf || null;
  }
});

// node_modules/.pnpm/get-proto@1.0.1/node_modules/get-proto/Object.getPrototypeOf.js
var require_Object_getPrototypeOf = __commonJS({
  "node_modules/.pnpm/get-proto@1.0.1/node_modules/get-proto/Object.getPrototypeOf.js"(exports2, module2) {
    "use strict";
    var $Object = require_es_object_atoms();
    module2.exports = $Object.getPrototypeOf || null;
  }
});

// node_modules/.pnpm/function-bind@1.1.2/node_modules/function-bind/implementation.js
var require_implementation = __commonJS({
  "node_modules/.pnpm/function-bind@1.1.2/node_modules/function-bind/implementation.js"(exports2, module2) {
    "use strict";
    var ERROR_MESSAGE = "Function.prototype.bind called on incompatible ";
    var toStr = Object.prototype.toString;
    var max = Math.max;
    var funcType = "[object Function]";
    var concatty = function concatty2(a, b) {
      var arr = [];
      for (var i = 0; i < a.length; i += 1) {
        arr[i] = a[i];
      }
      for (var j = 0; j < b.length; j += 1) {
        arr[j + a.length] = b[j];
      }
      return arr;
    };
    var slicy = function slicy2(arrLike, offset) {
      var arr = [];
      for (var i = offset || 0, j = 0; i < arrLike.length; i += 1, j += 1) {
        arr[j] = arrLike[i];
      }
      return arr;
    };
    var joiny = function(arr, joiner) {
      var str = "";
      for (var i = 0; i < arr.length; i += 1) {
        str += arr[i];
        if (i + 1 < arr.length) {
          str += joiner;
        }
      }
      return str;
    };
    module2.exports = function bind(that) {
      var target = this;
      if (typeof target !== "function" || toStr.apply(target) !== funcType) {
        throw new TypeError(ERROR_MESSAGE + target);
      }
      var args = slicy(arguments, 1);
      var bound;
      var binder = function() {
        if (this instanceof bound) {
          var result = target.apply(
            this,
            concatty(args, arguments)
          );
          if (Object(result) === result) {
            return result;
          }
          return this;
        }
        return target.apply(
          that,
          concatty(args, arguments)
        );
      };
      var boundLength = max(0, target.length - args.length);
      var boundArgs = [];
      for (var i = 0; i < boundLength; i++) {
        boundArgs[i] = "$" + i;
      }
      bound = Function("binder", "return function (" + joiny(boundArgs, ",") + "){ return binder.apply(this,arguments); }")(binder);
      if (target.prototype) {
        var Empty = function Empty2() {
        };
        Empty.prototype = target.prototype;
        bound.prototype = new Empty();
        Empty.prototype = null;
      }
      return bound;
    };
  }
});

// node_modules/.pnpm/function-bind@1.1.2/node_modules/function-bind/index.js
var require_function_bind = __commonJS({
  "node_modules/.pnpm/function-bind@1.1.2/node_modules/function-bind/index.js"(exports2, module2) {
    "use strict";
    var implementation = require_implementation();
    module2.exports = Function.prototype.bind || implementation;
  }
});

// node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/functionCall.js
var require_functionCall = __commonJS({
  "node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/functionCall.js"(exports2, module2) {
    "use strict";
    module2.exports = Function.prototype.call;
  }
});

// node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/functionApply.js
var require_functionApply = __commonJS({
  "node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/functionApply.js"(exports2, module2) {
    "use strict";
    module2.exports = Function.prototype.apply;
  }
});

// node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/reflectApply.js
var require_reflectApply = __commonJS({
  "node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/reflectApply.js"(exports2, module2) {
    "use strict";
    module2.exports = typeof Reflect !== "undefined" && Reflect && Reflect.apply;
  }
});

// node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/actualApply.js
var require_actualApply = __commonJS({
  "node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/actualApply.js"(exports2, module2) {
    "use strict";
    var bind = require_function_bind();
    var $apply = require_functionApply();
    var $call = require_functionCall();
    var $reflectApply = require_reflectApply();
    module2.exports = $reflectApply || bind.call($call, $apply);
  }
});

// node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/index.js
var require_call_bind_apply_helpers = __commonJS({
  "node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/index.js"(exports2, module2) {
    "use strict";
    var bind = require_function_bind();
    var $TypeError = require_type();
    var $call = require_functionCall();
    var $actualApply = require_actualApply();
    module2.exports = function callBindBasic(args) {
      if (args.length < 1 || typeof args[0] !== "function") {
        throw new $TypeError("a function is required");
      }
      return $actualApply(bind, $call, args);
    };
  }
});

// node_modules/.pnpm/dunder-proto@1.0.1/node_modules/dunder-proto/get.js
var require_get = __commonJS({
  "node_modules/.pnpm/dunder-proto@1.0.1/node_modules/dunder-proto/get.js"(exports2, module2) {
    "use strict";
    var callBind = require_call_bind_apply_helpers();
    var gOPD = require_gopd();
    var hasProtoAccessor;
    try {
      hasProtoAccessor = /** @type {{ __proto__?: typeof Array.prototype }} */
      [].__proto__ === Array.prototype;
    } catch (e) {
      if (!e || typeof e !== "object" || !("code" in e) || e.code !== "ERR_PROTO_ACCESS") {
        throw e;
      }
    }
    var desc = !!hasProtoAccessor && gOPD && gOPD(
      Object.prototype,
      /** @type {keyof typeof Object.prototype} */
      "__proto__"
    );
    var $Object = Object;
    var $getPrototypeOf = $Object.getPrototypeOf;
    module2.exports = desc && typeof desc.get === "function" ? callBind([desc.get]) : typeof $getPrototypeOf === "function" ? (
      /** @type {import('./get')} */
      function getDunder(value) {
        return $getPrototypeOf(value == null ? value : $Object(value));
      }
    ) : false;
  }
});

// node_modules/.pnpm/get-proto@1.0.1/node_modules/get-proto/index.js
var require_get_proto = __commonJS({
  "node_modules/.pnpm/get-proto@1.0.1/node_modules/get-proto/index.js"(exports2, module2) {
    "use strict";
    var reflectGetProto = require_Reflect_getPrototypeOf();
    var originalGetProto = require_Object_getPrototypeOf();
    var getDunderProto = require_get();
    module2.exports = reflectGetProto ? function getProto(O) {
      return reflectGetProto(O);
    } : originalGetProto ? function getProto(O) {
      if (!O || typeof O !== "object" && typeof O !== "function") {
        throw new TypeError("getProto: not an object");
      }
      return originalGetProto(O);
    } : getDunderProto ? function getProto(O) {
      return getDunderProto(O);
    } : null;
  }
});

// node_modules/.pnpm/hasown@2.0.2/node_modules/hasown/index.js
var require_hasown = __commonJS({
  "node_modules/.pnpm/hasown@2.0.2/node_modules/hasown/index.js"(exports2, module2) {
    "use strict";
    var call = Function.prototype.call;
    var $hasOwn = Object.prototype.hasOwnProperty;
    var bind = require_function_bind();
    module2.exports = bind.call(call, $hasOwn);
  }
});

// node_modules/.pnpm/get-intrinsic@1.3.0/node_modules/get-intrinsic/index.js
var require_get_intrinsic = __commonJS({
  "node_modules/.pnpm/get-intrinsic@1.3.0/node_modules/get-intrinsic/index.js"(exports2, module2) {
    "use strict";
    var undefined2;
    var $Object = require_es_object_atoms();
    var $Error = require_es_errors();
    var $EvalError = require_eval();
    var $RangeError = require_range();
    var $ReferenceError = require_ref();
    var $SyntaxError = require_syntax();
    var $TypeError = require_type();
    var $URIError = require_uri();
    var abs = require_abs();
    var floor = require_floor();
    var max = require_max();
    var min = require_min();
    var pow = require_pow();
    var round = require_round();
    var sign = require_sign();
    var $Function = Function;
    var getEvalledConstructor = function(expressionSyntax) {
      try {
        return $Function('"use strict"; return (' + expressionSyntax + ").constructor;")();
      } catch (e) {
      }
    };
    var $gOPD = require_gopd();
    var $defineProperty = require_es_define_property();
    var throwTypeError = function() {
      throw new $TypeError();
    };
    var ThrowTypeError = $gOPD ? (function() {
      try {
        arguments.callee;
        return throwTypeError;
      } catch (calleeThrows) {
        try {
          return $gOPD(arguments, "callee").get;
        } catch (gOPDthrows) {
          return throwTypeError;
        }
      }
    })() : throwTypeError;
    var hasSymbols = require_has_symbols()();
    var getProto = require_get_proto();
    var $ObjectGPO = require_Object_getPrototypeOf();
    var $ReflectGPO = require_Reflect_getPrototypeOf();
    var $apply = require_functionApply();
    var $call = require_functionCall();
    var needsEval = {};
    var TypedArray = typeof Uint8Array === "undefined" || !getProto ? undefined2 : getProto(Uint8Array);
    var INTRINSICS = {
      __proto__: null,
      "%AggregateError%": typeof AggregateError === "undefined" ? undefined2 : AggregateError,
      "%Array%": Array,
      "%ArrayBuffer%": typeof ArrayBuffer === "undefined" ? undefined2 : ArrayBuffer,
      "%ArrayIteratorPrototype%": hasSymbols && getProto ? getProto([][Symbol.iterator]()) : undefined2,
      "%AsyncFromSyncIteratorPrototype%": undefined2,
      "%AsyncFunction%": needsEval,
      "%AsyncGenerator%": needsEval,
      "%AsyncGeneratorFunction%": needsEval,
      "%AsyncIteratorPrototype%": needsEval,
      "%Atomics%": typeof Atomics === "undefined" ? undefined2 : Atomics,
      "%BigInt%": typeof BigInt === "undefined" ? undefined2 : BigInt,
      "%BigInt64Array%": typeof BigInt64Array === "undefined" ? undefined2 : BigInt64Array,
      "%BigUint64Array%": typeof BigUint64Array === "undefined" ? undefined2 : BigUint64Array,
      "%Boolean%": Boolean,
      "%DataView%": typeof DataView === "undefined" ? undefined2 : DataView,
      "%Date%": Date,
      "%decodeURI%": decodeURI,
      "%decodeURIComponent%": decodeURIComponent,
      "%encodeURI%": encodeURI,
      "%encodeURIComponent%": encodeURIComponent,
      "%Error%": $Error,
      "%eval%": eval,
      // eslint-disable-line no-eval
      "%EvalError%": $EvalError,
      "%Float16Array%": typeof Float16Array === "undefined" ? undefined2 : Float16Array,
      "%Float32Array%": typeof Float32Array === "undefined" ? undefined2 : Float32Array,
      "%Float64Array%": typeof Float64Array === "undefined" ? undefined2 : Float64Array,
      "%FinalizationRegistry%": typeof FinalizationRegistry === "undefined" ? undefined2 : FinalizationRegistry,
      "%Function%": $Function,
      "%GeneratorFunction%": needsEval,
      "%Int8Array%": typeof Int8Array === "undefined" ? undefined2 : Int8Array,
      "%Int16Array%": typeof Int16Array === "undefined" ? undefined2 : Int16Array,
      "%Int32Array%": typeof Int32Array === "undefined" ? undefined2 : Int32Array,
      "%isFinite%": isFinite,
      "%isNaN%": isNaN,
      "%IteratorPrototype%": hasSymbols && getProto ? getProto(getProto([][Symbol.iterator]())) : undefined2,
      "%JSON%": typeof JSON === "object" ? JSON : undefined2,
      "%Map%": typeof Map === "undefined" ? undefined2 : Map,
      "%MapIteratorPrototype%": typeof Map === "undefined" || !hasSymbols || !getProto ? undefined2 : getProto((/* @__PURE__ */ new Map())[Symbol.iterator]()),
      "%Math%": Math,
      "%Number%": Number,
      "%Object%": $Object,
      "%Object.getOwnPropertyDescriptor%": $gOPD,
      "%parseFloat%": parseFloat,
      "%parseInt%": parseInt,
      "%Promise%": typeof Promise === "undefined" ? undefined2 : Promise,
      "%Proxy%": typeof Proxy === "undefined" ? undefined2 : Proxy,
      "%RangeError%": $RangeError,
      "%ReferenceError%": $ReferenceError,
      "%Reflect%": typeof Reflect === "undefined" ? undefined2 : Reflect,
      "%RegExp%": RegExp,
      "%Set%": typeof Set === "undefined" ? undefined2 : Set,
      "%SetIteratorPrototype%": typeof Set === "undefined" || !hasSymbols || !getProto ? undefined2 : getProto((/* @__PURE__ */ new Set())[Symbol.iterator]()),
      "%SharedArrayBuffer%": typeof SharedArrayBuffer === "undefined" ? undefined2 : SharedArrayBuffer,
      "%String%": String,
      "%StringIteratorPrototype%": hasSymbols && getProto ? getProto(""[Symbol.iterator]()) : undefined2,
      "%Symbol%": hasSymbols ? Symbol : undefined2,
      "%SyntaxError%": $SyntaxError,
      "%ThrowTypeError%": ThrowTypeError,
      "%TypedArray%": TypedArray,
      "%TypeError%": $TypeError,
      "%Uint8Array%": typeof Uint8Array === "undefined" ? undefined2 : Uint8Array,
      "%Uint8ClampedArray%": typeof Uint8ClampedArray === "undefined" ? undefined2 : Uint8ClampedArray,
      "%Uint16Array%": typeof Uint16Array === "undefined" ? undefined2 : Uint16Array,
      "%Uint32Array%": typeof Uint32Array === "undefined" ? undefined2 : Uint32Array,
      "%URIError%": $URIError,
      "%WeakMap%": typeof WeakMap === "undefined" ? undefined2 : WeakMap,
      "%WeakRef%": typeof WeakRef === "undefined" ? undefined2 : WeakRef,
      "%WeakSet%": typeof WeakSet === "undefined" ? undefined2 : WeakSet,
      "%Function.prototype.call%": $call,
      "%Function.prototype.apply%": $apply,
      "%Object.defineProperty%": $defineProperty,
      "%Object.getPrototypeOf%": $ObjectGPO,
      "%Math.abs%": abs,
      "%Math.floor%": floor,
      "%Math.max%": max,
      "%Math.min%": min,
      "%Math.pow%": pow,
      "%Math.round%": round,
      "%Math.sign%": sign,
      "%Reflect.getPrototypeOf%": $ReflectGPO
    };
    if (getProto) {
      try {
        null.error;
      } catch (e) {
        errorProto = getProto(getProto(e));
        INTRINSICS["%Error.prototype%"] = errorProto;
      }
    }
    var errorProto;
    var doEval = function doEval2(name) {
      var value;
      if (name === "%AsyncFunction%") {
        value = getEvalledConstructor("async function () {}");
      } else if (name === "%GeneratorFunction%") {
        value = getEvalledConstructor("function* () {}");
      } else if (name === "%AsyncGeneratorFunction%") {
        value = getEvalledConstructor("async function* () {}");
      } else if (name === "%AsyncGenerator%") {
        var fn = doEval2("%AsyncGeneratorFunction%");
        if (fn) {
          value = fn.prototype;
        }
      } else if (name === "%AsyncIteratorPrototype%") {
        var gen = doEval2("%AsyncGenerator%");
        if (gen && getProto) {
          value = getProto(gen.prototype);
        }
      }
      INTRINSICS[name] = value;
      return value;
    };
    var LEGACY_ALIASES = {
      __proto__: null,
      "%ArrayBufferPrototype%": ["ArrayBuffer", "prototype"],
      "%ArrayPrototype%": ["Array", "prototype"],
      "%ArrayProto_entries%": ["Array", "prototype", "entries"],
      "%ArrayProto_forEach%": ["Array", "prototype", "forEach"],
      "%ArrayProto_keys%": ["Array", "prototype", "keys"],
      "%ArrayProto_values%": ["Array", "prototype", "values"],
      "%AsyncFunctionPrototype%": ["AsyncFunction", "prototype"],
      "%AsyncGenerator%": ["AsyncGeneratorFunction", "prototype"],
      "%AsyncGeneratorPrototype%": ["AsyncGeneratorFunction", "prototype", "prototype"],
      "%BooleanPrototype%": ["Boolean", "prototype"],
      "%DataViewPrototype%": ["DataView", "prototype"],
      "%DatePrototype%": ["Date", "prototype"],
      "%ErrorPrototype%": ["Error", "prototype"],
      "%EvalErrorPrototype%": ["EvalError", "prototype"],
      "%Float32ArrayPrototype%": ["Float32Array", "prototype"],
      "%Float64ArrayPrototype%": ["Float64Array", "prototype"],
      "%FunctionPrototype%": ["Function", "prototype"],
      "%Generator%": ["GeneratorFunction", "prototype"],
      "%GeneratorPrototype%": ["GeneratorFunction", "prototype", "prototype"],
      "%Int8ArrayPrototype%": ["Int8Array", "prototype"],
      "%Int16ArrayPrototype%": ["Int16Array", "prototype"],
      "%Int32ArrayPrototype%": ["Int32Array", "prototype"],
      "%JSONParse%": ["JSON", "parse"],
      "%JSONStringify%": ["JSON", "stringify"],
      "%MapPrototype%": ["Map", "prototype"],
      "%NumberPrototype%": ["Number", "prototype"],
      "%ObjectPrototype%": ["Object", "prototype"],
      "%ObjProto_toString%": ["Object", "prototype", "toString"],
      "%ObjProto_valueOf%": ["Object", "prototype", "valueOf"],
      "%PromisePrototype%": ["Promise", "prototype"],
      "%PromiseProto_then%": ["Promise", "prototype", "then"],
      "%Promise_all%": ["Promise", "all"],
      "%Promise_reject%": ["Promise", "reject"],
      "%Promise_resolve%": ["Promise", "resolve"],
      "%RangeErrorPrototype%": ["RangeError", "prototype"],
      "%ReferenceErrorPrototype%": ["ReferenceError", "prototype"],
      "%RegExpPrototype%": ["RegExp", "prototype"],
      "%SetPrototype%": ["Set", "prototype"],
      "%SharedArrayBufferPrototype%": ["SharedArrayBuffer", "prototype"],
      "%StringPrototype%": ["String", "prototype"],
      "%SymbolPrototype%": ["Symbol", "prototype"],
      "%SyntaxErrorPrototype%": ["SyntaxError", "prototype"],
      "%TypedArrayPrototype%": ["TypedArray", "prototype"],
      "%TypeErrorPrototype%": ["TypeError", "prototype"],
      "%Uint8ArrayPrototype%": ["Uint8Array", "prototype"],
      "%Uint8ClampedArrayPrototype%": ["Uint8ClampedArray", "prototype"],
      "%Uint16ArrayPrototype%": ["Uint16Array", "prototype"],
      "%Uint32ArrayPrototype%": ["Uint32Array", "prototype"],
      "%URIErrorPrototype%": ["URIError", "prototype"],
      "%WeakMapPrototype%": ["WeakMap", "prototype"],
      "%WeakSetPrototype%": ["WeakSet", "prototype"]
    };
    var bind = require_function_bind();
    var hasOwn = require_hasown();
    var $concat = bind.call($call, Array.prototype.concat);
    var $spliceApply = bind.call($apply, Array.prototype.splice);
    var $replace = bind.call($call, String.prototype.replace);
    var $strSlice = bind.call($call, String.prototype.slice);
    var $exec = bind.call($call, RegExp.prototype.exec);
    var rePropName = /[^%.[\\]]+|\\[(?:(-?\\d+(?:\\.\\d+)?)|(["'])((?:(?!\\2)[^\\\\]|\\\\.)*?)\\2)\\]|(?=(?:\\.|\\[\\])(?:\\.|\\[\\]|%$))/g;
    var reEscapeChar = /\\\\(\\\\)?/g;
    var stringToPath = function stringToPath2(string) {
      var first = $strSlice(string, 0, 1);
      var last = $strSlice(string, -1);
      if (first === "%" && last !== "%") {
        throw new $SyntaxError("invalid intrinsic syntax, expected closing \`%\`");
      } else if (last === "%" && first !== "%") {
        throw new $SyntaxError("invalid intrinsic syntax, expected opening \`%\`");
      }
      var result = [];
      $replace(string, rePropName, function(match, number, quote, subString) {
        result[result.length] = quote ? $replace(subString, reEscapeChar, "$1") : number || match;
      });
      return result;
    };
    var getBaseIntrinsic = function getBaseIntrinsic2(name, allowMissing) {
      var intrinsicName = name;
      var alias;
      if (hasOwn(LEGACY_ALIASES, intrinsicName)) {
        alias = LEGACY_ALIASES[intrinsicName];
        intrinsicName = "%" + alias[0] + "%";
      }
      if (hasOwn(INTRINSICS, intrinsicName)) {
        var value = INTRINSICS[intrinsicName];
        if (value === needsEval) {
          value = doEval(intrinsicName);
        }
        if (typeof value === "undefined" && !allowMissing) {
          throw new $TypeError("intrinsic " + name + " exists, but is not available. Please file an issue!");
        }
        return {
          alias,
          name: intrinsicName,
          value
        };
      }
      throw new $SyntaxError("intrinsic " + name + " does not exist!");
    };
    module2.exports = function GetIntrinsic(name, allowMissing) {
      if (typeof name !== "string" || name.length === 0) {
        throw new $TypeError("intrinsic name must be a non-empty string");
      }
      if (arguments.length > 1 && typeof allowMissing !== "boolean") {
        throw new $TypeError('"allowMissing" argument must be a boolean');
      }
      if ($exec(/^%?[^%]*%?$/, name) === null) {
        throw new $SyntaxError("\`%\` may not be present anywhere but at the beginning and end of the intrinsic name");
      }
      var parts = stringToPath(name);
      var intrinsicBaseName = parts.length > 0 ? parts[0] : "";
      var intrinsic = getBaseIntrinsic("%" + intrinsicBaseName + "%", allowMissing);
      var intrinsicRealName = intrinsic.name;
      var value = intrinsic.value;
      var skipFurtherCaching = false;
      var alias = intrinsic.alias;
      if (alias) {
        intrinsicBaseName = alias[0];
        $spliceApply(parts, $concat([0, 1], alias));
      }
      for (var i = 1, isOwn = true; i < parts.length; i += 1) {
        var part = parts[i];
        var first = $strSlice(part, 0, 1);
        var last = $strSlice(part, -1);
        if ((first === '"' || first === "'" || first === "\`" || (last === '"' || last === "'" || last === "\`")) && first !== last) {
          throw new $SyntaxError("property names with quotes must have matching quotes");
        }
        if (part === "constructor" || !isOwn) {
          skipFurtherCaching = true;
        }
        intrinsicBaseName += "." + part;
        intrinsicRealName = "%" + intrinsicBaseName + "%";
        if (hasOwn(INTRINSICS, intrinsicRealName)) {
          value = INTRINSICS[intrinsicRealName];
        } else if (value != null) {
          if (!(part in value)) {
            if (!allowMissing) {
              throw new $TypeError("base intrinsic for " + name + " exists, but the property is not available.");
            }
            return void undefined2;
          }
          if ($gOPD && i + 1 >= parts.length) {
            var desc = $gOPD(value, part);
            isOwn = !!desc;
            if (isOwn && "get" in desc && !("originalValue" in desc.get)) {
              value = desc.get;
            } else {
              value = value[part];
            }
          } else {
            isOwn = hasOwn(value, part);
            value = value[part];
          }
          if (isOwn && !skipFurtherCaching) {
            INTRINSICS[intrinsicRealName] = value;
          }
        }
      }
      return value;
    };
  }
});

// node_modules/.pnpm/call-bound@1.0.4/node_modules/call-bound/index.js
var require_call_bound = __commonJS({
  "node_modules/.pnpm/call-bound@1.0.4/node_modules/call-bound/index.js"(exports2, module2) {
    "use strict";
    var GetIntrinsic = require_get_intrinsic();
    var callBindBasic = require_call_bind_apply_helpers();
    var $indexOf = callBindBasic([GetIntrinsic("%String.prototype.indexOf%")]);
    module2.exports = function callBoundIntrinsic(name, allowMissing) {
      var intrinsic = (
        /** @type {(this: unknown, ...args: unknown[]) => unknown} */
        GetIntrinsic(name, !!allowMissing)
      );
      if (typeof intrinsic === "function" && $indexOf(name, ".prototype.") > -1) {
        return callBindBasic(
          /** @type {const} */
          [intrinsic]
        );
      }
      return intrinsic;
    };
  }
});

// node_modules/.pnpm/is-arguments@1.2.0/node_modules/is-arguments/index.js
var require_is_arguments = __commonJS({
  "node_modules/.pnpm/is-arguments@1.2.0/node_modules/is-arguments/index.js"(exports2, module2) {
    "use strict";
    var hasToStringTag = require_shams2()();
    var callBound = require_call_bound();
    var $toString = callBound("Object.prototype.toString");
    var isStandardArguments = function isArguments(value) {
      if (hasToStringTag && value && typeof value === "object" && Symbol.toStringTag in value) {
        return false;
      }
      return $toString(value) === "[object Arguments]";
    };
    var isLegacyArguments = function isArguments(value) {
      if (isStandardArguments(value)) {
        return true;
      }
      return value !== null && typeof value === "object" && "length" in value && typeof value.length === "number" && value.length >= 0 && $toString(value) !== "[object Array]" && "callee" in value && $toString(value.callee) === "[object Function]";
    };
    var supportsStandardArguments = (function() {
      return isStandardArguments(arguments);
    })();
    isStandardArguments.isLegacyArguments = isLegacyArguments;
    module2.exports = supportsStandardArguments ? isStandardArguments : isLegacyArguments;
  }
});

// node_modules/.pnpm/is-regex@1.2.1/node_modules/is-regex/index.js
var require_is_regex = __commonJS({
  "node_modules/.pnpm/is-regex@1.2.1/node_modules/is-regex/index.js"(exports2, module2) {
    "use strict";
    var callBound = require_call_bound();
    var hasToStringTag = require_shams2()();
    var hasOwn = require_hasown();
    var gOPD = require_gopd();
    var fn;
    if (hasToStringTag) {
      $exec = callBound("RegExp.prototype.exec");
      isRegexMarker = {};
      throwRegexMarker = function() {
        throw isRegexMarker;
      };
      badStringifier = {
        toString: throwRegexMarker,
        valueOf: throwRegexMarker
      };
      if (typeof Symbol.toPrimitive === "symbol") {
        badStringifier[Symbol.toPrimitive] = throwRegexMarker;
      }
      fn = function isRegex(value) {
        if (!value || typeof value !== "object") {
          return false;
        }
        var descriptor = (
          /** @type {NonNullable<typeof gOPD>} */
          gOPD(
            /** @type {{ lastIndex?: unknown }} */
            value,
            "lastIndex"
          )
        );
        var hasLastIndexDataProperty = descriptor && hasOwn(descriptor, "value");
        if (!hasLastIndexDataProperty) {
          return false;
        }
        try {
          $exec(
            value,
            /** @type {string} */
            /** @type {unknown} */
            badStringifier
          );
        } catch (e) {
          return e === isRegexMarker;
        }
      };
    } else {
      $toString = callBound("Object.prototype.toString");
      regexClass = "[object RegExp]";
      fn = function isRegex(value) {
        if (!value || typeof value !== "object" && typeof value !== "function") {
          return false;
        }
        return $toString(value) === regexClass;
      };
    }
    var $exec;
    var isRegexMarker;
    var throwRegexMarker;
    var badStringifier;
    var $toString;
    var regexClass;
    module2.exports = fn;
  }
});

// node_modules/.pnpm/safe-regex-test@1.1.0/node_modules/safe-regex-test/index.js
var require_safe_regex_test = __commonJS({
  "node_modules/.pnpm/safe-regex-test@1.1.0/node_modules/safe-regex-test/index.js"(exports2, module2) {
    "use strict";
    var callBound = require_call_bound();
    var isRegex = require_is_regex();
    var $exec = callBound("RegExp.prototype.exec");
    var $TypeError = require_type();
    module2.exports = function regexTester(regex) {
      if (!isRegex(regex)) {
        throw new $TypeError("\`regex\` must be a RegExp");
      }
      return function test(s) {
        return $exec(regex, s) !== null;
      };
    };
  }
});

// node_modules/.pnpm/generator-function@2.0.1/node_modules/generator-function/index.js
var require_generator_function = __commonJS({
  "node_modules/.pnpm/generator-function@2.0.1/node_modules/generator-function/index.js"(exports2, module2) {
    "use strict";
    var cached = (
      /** @type {GeneratorFunctionConstructor} */
      function* () {
      }.constructor
    );
    module2.exports = () => cached;
  }
});

// node_modules/.pnpm/is-generator-function@1.1.2/node_modules/is-generator-function/index.js
var require_is_generator_function = __commonJS({
  "node_modules/.pnpm/is-generator-function@1.1.2/node_modules/is-generator-function/index.js"(exports2, module2) {
    "use strict";
    var callBound = require_call_bound();
    var safeRegexTest = require_safe_regex_test();
    var isFnRegex = safeRegexTest(/^\\s*(?:function)?\\*/);
    var hasToStringTag = require_shams2()();
    var getProto = require_get_proto();
    var toStr = callBound("Object.prototype.toString");
    var fnToStr = callBound("Function.prototype.toString");
    var getGeneratorFunction = require_generator_function();
    module2.exports = function isGeneratorFunction(fn) {
      if (typeof fn !== "function") {
        return false;
      }
      if (isFnRegex(fnToStr(fn))) {
        return true;
      }
      if (!hasToStringTag) {
        var str = toStr(fn);
        return str === "[object GeneratorFunction]";
      }
      if (!getProto) {
        return false;
      }
      var GeneratorFunction = getGeneratorFunction();
      return GeneratorFunction && getProto(fn) === GeneratorFunction.prototype;
    };
  }
});

// node_modules/.pnpm/is-callable@1.2.7/node_modules/is-callable/index.js
var require_is_callable = __commonJS({
  "node_modules/.pnpm/is-callable@1.2.7/node_modules/is-callable/index.js"(exports2, module2) {
    "use strict";
    var fnToStr = Function.prototype.toString;
    var reflectApply = typeof Reflect === "object" && Reflect !== null && Reflect.apply;
    var badArrayLike;
    var isCallableMarker;
    if (typeof reflectApply === "function" && typeof Object.defineProperty === "function") {
      try {
        badArrayLike = Object.defineProperty({}, "length", {
          get: function() {
            throw isCallableMarker;
          }
        });
        isCallableMarker = {};
        reflectApply(function() {
          throw 42;
        }, null, badArrayLike);
      } catch (_) {
        if (_ !== isCallableMarker) {
          reflectApply = null;
        }
      }
    } else {
      reflectApply = null;
    }
    var constructorRegex = /^\\s*class\\b/;
    var isES6ClassFn = function isES6ClassFunction(value) {
      try {
        var fnStr = fnToStr.call(value);
        return constructorRegex.test(fnStr);
      } catch (e) {
        return false;
      }
    };
    var tryFunctionObject = function tryFunctionToStr(value) {
      try {
        if (isES6ClassFn(value)) {
          return false;
        }
        fnToStr.call(value);
        return true;
      } catch (e) {
        return false;
      }
    };
    var toStr = Object.prototype.toString;
    var objectClass = "[object Object]";
    var fnClass = "[object Function]";
    var genClass = "[object GeneratorFunction]";
    var ddaClass = "[object HTMLAllCollection]";
    var ddaClass2 = "[object HTML document.all class]";
    var ddaClass3 = "[object HTMLCollection]";
    var hasToStringTag = typeof Symbol === "function" && !!Symbol.toStringTag;
    var isIE68 = !(0 in [,]);
    var isDDA = function isDocumentDotAll() {
      return false;
    };
    if (typeof document === "object") {
      all = document.all;
      if (toStr.call(all) === toStr.call(document.all)) {
        isDDA = function isDocumentDotAll(value) {
          if ((isIE68 || !value) && (typeof value === "undefined" || typeof value === "object")) {
            try {
              var str = toStr.call(value);
              return (str === ddaClass || str === ddaClass2 || str === ddaClass3 || str === objectClass) && value("") == null;
            } catch (e) {
            }
          }
          return false;
        };
      }
    }
    var all;
    module2.exports = reflectApply ? function isCallable(value) {
      if (isDDA(value)) {
        return true;
      }
      if (!value) {
        return false;
      }
      if (typeof value !== "function" && typeof value !== "object") {
        return false;
      }
      try {
        reflectApply(value, null, badArrayLike);
      } catch (e) {
        if (e !== isCallableMarker) {
          return false;
        }
      }
      return !isES6ClassFn(value) && tryFunctionObject(value);
    } : function isCallable(value) {
      if (isDDA(value)) {
        return true;
      }
      if (!value) {
        return false;
      }
      if (typeof value !== "function" && typeof value !== "object") {
        return false;
      }
      if (hasToStringTag) {
        return tryFunctionObject(value);
      }
      if (isES6ClassFn(value)) {
        return false;
      }
      var strClass = toStr.call(value);
      if (strClass !== fnClass && strClass !== genClass && !/^\\[object HTML/.test(strClass)) {
        return false;
      }
      return tryFunctionObject(value);
    };
  }
});

// node_modules/.pnpm/for-each@0.3.5/node_modules/for-each/index.js
var require_for_each = __commonJS({
  "node_modules/.pnpm/for-each@0.3.5/node_modules/for-each/index.js"(exports2, module2) {
    "use strict";
    var isCallable = require_is_callable();
    var toStr = Object.prototype.toString;
    var hasOwnProperty = Object.prototype.hasOwnProperty;
    var forEachArray = function forEachArray2(array, iterator, receiver) {
      for (var i = 0, len = array.length; i < len; i++) {
        if (hasOwnProperty.call(array, i)) {
          if (receiver == null) {
            iterator(array[i], i, array);
          } else {
            iterator.call(receiver, array[i], i, array);
          }
        }
      }
    };
    var forEachString = function forEachString2(string, iterator, receiver) {
      for (var i = 0, len = string.length; i < len; i++) {
        if (receiver == null) {
          iterator(string.charAt(i), i, string);
        } else {
          iterator.call(receiver, string.charAt(i), i, string);
        }
      }
    };
    var forEachObject = function forEachObject2(object, iterator, receiver) {
      for (var k in object) {
        if (hasOwnProperty.call(object, k)) {
          if (receiver == null) {
            iterator(object[k], k, object);
          } else {
            iterator.call(receiver, object[k], k, object);
          }
        }
      }
    };
    function isArray(x) {
      return toStr.call(x) === "[object Array]";
    }
    module2.exports = function forEach(list, iterator, thisArg) {
      if (!isCallable(iterator)) {
        throw new TypeError("iterator must be a function");
      }
      var receiver;
      if (arguments.length >= 3) {
        receiver = thisArg;
      }
      if (isArray(list)) {
        forEachArray(list, iterator, receiver);
      } else if (typeof list === "string") {
        forEachString(list, iterator, receiver);
      } else {
        forEachObject(list, iterator, receiver);
      }
    };
  }
});

// node_modules/.pnpm/possible-typed-array-names@1.1.0/node_modules/possible-typed-array-names/index.js
var require_possible_typed_array_names = __commonJS({
  "node_modules/.pnpm/possible-typed-array-names@1.1.0/node_modules/possible-typed-array-names/index.js"(exports2, module2) {
    "use strict";
    module2.exports = [
      "Float16Array",
      "Float32Array",
      "Float64Array",
      "Int8Array",
      "Int16Array",
      "Int32Array",
      "Uint8Array",
      "Uint8ClampedArray",
      "Uint16Array",
      "Uint32Array",
      "BigInt64Array",
      "BigUint64Array"
    ];
  }
});

// node_modules/.pnpm/available-typed-arrays@1.0.7/node_modules/available-typed-arrays/index.js
var require_available_typed_arrays = __commonJS({
  "node_modules/.pnpm/available-typed-arrays@1.0.7/node_modules/available-typed-arrays/index.js"(exports2, module2) {
    "use strict";
    var possibleNames = require_possible_typed_array_names();
    var g = typeof globalThis === "undefined" ? global : globalThis;
    module2.exports = function availableTypedArrays() {
      var out = [];
      for (var i = 0; i < possibleNames.length; i++) {
        if (typeof g[possibleNames[i]] === "function") {
          out[out.length] = possibleNames[i];
        }
      }
      return out;
    };
  }
});

// node_modules/.pnpm/define-data-property@1.1.4/node_modules/define-data-property/index.js
var require_define_data_property = __commonJS({
  "node_modules/.pnpm/define-data-property@1.1.4/node_modules/define-data-property/index.js"(exports2, module2) {
    "use strict";
    var $defineProperty = require_es_define_property();
    var $SyntaxError = require_syntax();
    var $TypeError = require_type();
    var gopd = require_gopd();
    module2.exports = function defineDataProperty(obj, property, value) {
      if (!obj || typeof obj !== "object" && typeof obj !== "function") {
        throw new $TypeError("\`obj\` must be an object or a function\`");
      }
      if (typeof property !== "string" && typeof property !== "symbol") {
        throw new $TypeError("\`property\` must be a string or a symbol\`");
      }
      if (arguments.length > 3 && typeof arguments[3] !== "boolean" && arguments[3] !== null) {
        throw new $TypeError("\`nonEnumerable\`, if provided, must be a boolean or null");
      }
      if (arguments.length > 4 && typeof arguments[4] !== "boolean" && arguments[4] !== null) {
        throw new $TypeError("\`nonWritable\`, if provided, must be a boolean or null");
      }
      if (arguments.length > 5 && typeof arguments[5] !== "boolean" && arguments[5] !== null) {
        throw new $TypeError("\`nonConfigurable\`, if provided, must be a boolean or null");
      }
      if (arguments.length > 6 && typeof arguments[6] !== "boolean") {
        throw new $TypeError("\`loose\`, if provided, must be a boolean");
      }
      var nonEnumerable = arguments.length > 3 ? arguments[3] : null;
      var nonWritable = arguments.length > 4 ? arguments[4] : null;
      var nonConfigurable = arguments.length > 5 ? arguments[5] : null;
      var loose = arguments.length > 6 ? arguments[6] : false;
      var desc = !!gopd && gopd(obj, property);
      if ($defineProperty) {
        $defineProperty(obj, property, {
          configurable: nonConfigurable === null && desc ? desc.configurable : !nonConfigurable,
          enumerable: nonEnumerable === null && desc ? desc.enumerable : !nonEnumerable,
          value,
          writable: nonWritable === null && desc ? desc.writable : !nonWritable
        });
      } else if (loose || !nonEnumerable && !nonWritable && !nonConfigurable) {
        obj[property] = value;
      } else {
        throw new $SyntaxError("This environment does not support defining a property as non-configurable, non-writable, or non-enumerable.");
      }
    };
  }
});

// node_modules/.pnpm/has-property-descriptors@1.0.2/node_modules/has-property-descriptors/index.js
var require_has_property_descriptors = __commonJS({
  "node_modules/.pnpm/has-property-descriptors@1.0.2/node_modules/has-property-descriptors/index.js"(exports2, module2) {
    "use strict";
    var $defineProperty = require_es_define_property();
    var hasPropertyDescriptors = function hasPropertyDescriptors2() {
      return !!$defineProperty;
    };
    hasPropertyDescriptors.hasArrayLengthDefineBug = function hasArrayLengthDefineBug() {
      if (!$defineProperty) {
        return null;
      }
      try {
        return $defineProperty([], "length", { value: 1 }).length !== 1;
      } catch (e) {
        return true;
      }
    };
    module2.exports = hasPropertyDescriptors;
  }
});

// node_modules/.pnpm/set-function-length@1.2.2/node_modules/set-function-length/index.js
var require_set_function_length = __commonJS({
  "node_modules/.pnpm/set-function-length@1.2.2/node_modules/set-function-length/index.js"(exports2, module2) {
    "use strict";
    var GetIntrinsic = require_get_intrinsic();
    var define = require_define_data_property();
    var hasDescriptors = require_has_property_descriptors()();
    var gOPD = require_gopd();
    var $TypeError = require_type();
    var $floor = GetIntrinsic("%Math.floor%");
    module2.exports = function setFunctionLength(fn, length) {
      if (typeof fn !== "function") {
        throw new $TypeError("\`fn\` is not a function");
      }
      if (typeof length !== "number" || length < 0 || length > 4294967295 || $floor(length) !== length) {
        throw new $TypeError("\`length\` must be a positive 32-bit integer");
      }
      var loose = arguments.length > 2 && !!arguments[2];
      var functionLengthIsConfigurable = true;
      var functionLengthIsWritable = true;
      if ("length" in fn && gOPD) {
        var desc = gOPD(fn, "length");
        if (desc && !desc.configurable) {
          functionLengthIsConfigurable = false;
        }
        if (desc && !desc.writable) {
          functionLengthIsWritable = false;
        }
      }
      if (functionLengthIsConfigurable || functionLengthIsWritable || !loose) {
        if (hasDescriptors) {
          define(
            /** @type {Parameters<define>[0]} */
            fn,
            "length",
            length,
            true,
            true
          );
        } else {
          define(
            /** @type {Parameters<define>[0]} */
            fn,
            "length",
            length
          );
        }
      }
      return fn;
    };
  }
});

// node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/applyBind.js
var require_applyBind = __commonJS({
  "node_modules/.pnpm/call-bind-apply-helpers@1.0.2/node_modules/call-bind-apply-helpers/applyBind.js"(exports2, module2) {
    "use strict";
    var bind = require_function_bind();
    var $apply = require_functionApply();
    var actualApply = require_actualApply();
    module2.exports = function applyBind() {
      return actualApply(bind, $apply, arguments);
    };
  }
});

// node_modules/.pnpm/call-bind@1.0.8/node_modules/call-bind/index.js
var require_call_bind = __commonJS({
  "node_modules/.pnpm/call-bind@1.0.8/node_modules/call-bind/index.js"(exports2, module2) {
    "use strict";
    var setFunctionLength = require_set_function_length();
    var $defineProperty = require_es_define_property();
    var callBindBasic = require_call_bind_apply_helpers();
    var applyBind = require_applyBind();
    module2.exports = function callBind(originalFunction) {
      var func = callBindBasic(arguments);
      var adjustedLength = originalFunction.length - (arguments.length - 1);
      return setFunctionLength(
        func,
        1 + (adjustedLength > 0 ? adjustedLength : 0),
        true
      );
    };
    if ($defineProperty) {
      $defineProperty(module2.exports, "apply", { value: applyBind });
    } else {
      module2.exports.apply = applyBind;
    }
  }
});

// node_modules/.pnpm/which-typed-array@1.1.20/node_modules/which-typed-array/index.js
var require_which_typed_array = __commonJS({
  "node_modules/.pnpm/which-typed-array@1.1.20/node_modules/which-typed-array/index.js"(exports2, module2) {
    "use strict";
    var forEach = require_for_each();
    var availableTypedArrays = require_available_typed_arrays();
    var callBind = require_call_bind();
    var callBound = require_call_bound();
    var gOPD = require_gopd();
    var getProto = require_get_proto();
    var $toString = callBound("Object.prototype.toString");
    var hasToStringTag = require_shams2()();
    var g = typeof globalThis === "undefined" ? global : globalThis;
    var typedArrays = availableTypedArrays();
    var $slice = callBound("String.prototype.slice");
    var $indexOf = callBound("Array.prototype.indexOf", true) || function indexOf(array, value) {
      for (var i = 0; i < array.length; i += 1) {
        if (array[i] === value) {
          return i;
        }
      }
      return -1;
    };
    var cache = { __proto__: null };
    if (hasToStringTag && gOPD && getProto) {
      forEach(typedArrays, function(typedArray) {
        var arr = new g[typedArray]();
        if (Symbol.toStringTag in arr && getProto) {
          var proto = getProto(arr);
          var descriptor = gOPD(proto, Symbol.toStringTag);
          if (!descriptor && proto) {
            var superProto = getProto(proto);
            descriptor = gOPD(superProto, Symbol.toStringTag);
          }
          if (descriptor && descriptor.get) {
            var bound = callBind(descriptor.get);
            cache[
              /** @type {\`$\${import('.').TypedArrayName}\`} */
              "$" + typedArray
            ] = bound;
          }
        }
      });
    } else {
      forEach(typedArrays, function(typedArray) {
        var arr = new g[typedArray]();
        var fn = arr.slice || arr.set;
        if (fn) {
          var bound = (
            /** @type {import('./types').BoundSlice | import('./types').BoundSet} */
            // @ts-expect-error TODO FIXME
            callBind(fn)
          );
          cache[
            /** @type {\`$\${import('.').TypedArrayName}\`} */
            "$" + typedArray
          ] = bound;
        }
      });
    }
    var tryTypedArrays = function tryAllTypedArrays(value) {
      var found = false;
      forEach(
        /** @type {Record<\`\\$\${import('.').TypedArrayName}\`, Getter>} */
        cache,
        /** @type {(getter: Getter, name: \`\\$\${import('.').TypedArrayName}\`) => void} */
        function(getter, typedArray) {
          if (!found) {
            try {
              if ("$" + getter(value) === typedArray) {
                found = /** @type {import('.').TypedArrayName} */
                $slice(typedArray, 1);
              }
            } catch (e) {
            }
          }
        }
      );
      return found;
    };
    var trySlices = function tryAllSlices(value) {
      var found = false;
      forEach(
        /** @type {Record<\`\\$\${import('.').TypedArrayName}\`, Getter>} */
        cache,
        /** @type {(getter: Getter, name: \`\\$\${import('.').TypedArrayName}\`) => void} */
        function(getter, name) {
          if (!found) {
            try {
              getter(value);
              found = /** @type {import('.').TypedArrayName} */
              $slice(name, 1);
            } catch (e) {
            }
          }
        }
      );
      return found;
    };
    module2.exports = function whichTypedArray(value) {
      if (!value || typeof value !== "object") {
        return false;
      }
      if (!hasToStringTag) {
        var tag = $slice($toString(value), 8, -1);
        if ($indexOf(typedArrays, tag) > -1) {
          return tag;
        }
        if (tag !== "Object") {
          return false;
        }
        return trySlices(value);
      }
      if (!gOPD) {
        return null;
      }
      return tryTypedArrays(value);
    };
  }
});

// node_modules/.pnpm/is-typed-array@1.1.15/node_modules/is-typed-array/index.js
var require_is_typed_array = __commonJS({
  "node_modules/.pnpm/is-typed-array@1.1.15/node_modules/is-typed-array/index.js"(exports2, module2) {
    "use strict";
    var whichTypedArray = require_which_typed_array();
    module2.exports = function isTypedArray(value) {
      return !!whichTypedArray(value);
    };
  }
});

// node_modules/.pnpm/util@0.12.5/node_modules/util/support/types.js
var require_types = __commonJS({
  "node_modules/.pnpm/util@0.12.5/node_modules/util/support/types.js"(exports2) {
    "use strict";
    var isArgumentsObject = require_is_arguments();
    var isGeneratorFunction = require_is_generator_function();
    var whichTypedArray = require_which_typed_array();
    var isTypedArray = require_is_typed_array();
    function uncurryThis(f) {
      return f.call.bind(f);
    }
    var BigIntSupported = typeof BigInt !== "undefined";
    var SymbolSupported = typeof Symbol !== "undefined";
    var ObjectToString = uncurryThis(Object.prototype.toString);
    var numberValue = uncurryThis(Number.prototype.valueOf);
    var stringValue = uncurryThis(String.prototype.valueOf);
    var booleanValue = uncurryThis(Boolean.prototype.valueOf);
    if (BigIntSupported) {
      bigIntValue = uncurryThis(BigInt.prototype.valueOf);
    }
    var bigIntValue;
    if (SymbolSupported) {
      symbolValue = uncurryThis(Symbol.prototype.valueOf);
    }
    var symbolValue;
    function checkBoxedPrimitive(value, prototypeValueOf) {
      if (typeof value !== "object") {
        return false;
      }
      try {
        prototypeValueOf(value);
        return true;
      } catch (e) {
        return false;
      }
    }
    exports2.isArgumentsObject = isArgumentsObject;
    exports2.isGeneratorFunction = isGeneratorFunction;
    exports2.isTypedArray = isTypedArray;
    function isPromise(input) {
      return typeof Promise !== "undefined" && input instanceof Promise || input !== null && typeof input === "object" && typeof input.then === "function" && typeof input.catch === "function";
    }
    exports2.isPromise = isPromise;
    function isArrayBufferView(value) {
      if (typeof ArrayBuffer !== "undefined" && ArrayBuffer.isView) {
        return ArrayBuffer.isView(value);
      }
      return isTypedArray(value) || isDataView(value);
    }
    exports2.isArrayBufferView = isArrayBufferView;
    function isUint8Array(value) {
      return whichTypedArray(value) === "Uint8Array";
    }
    exports2.isUint8Array = isUint8Array;
    function isUint8ClampedArray(value) {
      return whichTypedArray(value) === "Uint8ClampedArray";
    }
    exports2.isUint8ClampedArray = isUint8ClampedArray;
    function isUint16Array(value) {
      return whichTypedArray(value) === "Uint16Array";
    }
    exports2.isUint16Array = isUint16Array;
    function isUint32Array(value) {
      return whichTypedArray(value) === "Uint32Array";
    }
    exports2.isUint32Array = isUint32Array;
    function isInt8Array(value) {
      return whichTypedArray(value) === "Int8Array";
    }
    exports2.isInt8Array = isInt8Array;
    function isInt16Array(value) {
      return whichTypedArray(value) === "Int16Array";
    }
    exports2.isInt16Array = isInt16Array;
    function isInt32Array(value) {
      return whichTypedArray(value) === "Int32Array";
    }
    exports2.isInt32Array = isInt32Array;
    function isFloat32Array(value) {
      return whichTypedArray(value) === "Float32Array";
    }
    exports2.isFloat32Array = isFloat32Array;
    function isFloat64Array(value) {
      return whichTypedArray(value) === "Float64Array";
    }
    exports2.isFloat64Array = isFloat64Array;
    function isBigInt64Array(value) {
      return whichTypedArray(value) === "BigInt64Array";
    }
    exports2.isBigInt64Array = isBigInt64Array;
    function isBigUint64Array(value) {
      return whichTypedArray(value) === "BigUint64Array";
    }
    exports2.isBigUint64Array = isBigUint64Array;
    function isMapToString(value) {
      return ObjectToString(value) === "[object Map]";
    }
    isMapToString.working = typeof Map !== "undefined" && isMapToString(/* @__PURE__ */ new Map());
    function isMap(value) {
      if (typeof Map === "undefined") {
        return false;
      }
      return isMapToString.working ? isMapToString(value) : value instanceof Map;
    }
    exports2.isMap = isMap;
    function isSetToString(value) {
      return ObjectToString(value) === "[object Set]";
    }
    isSetToString.working = typeof Set !== "undefined" && isSetToString(/* @__PURE__ */ new Set());
    function isSet(value) {
      if (typeof Set === "undefined") {
        return false;
      }
      return isSetToString.working ? isSetToString(value) : value instanceof Set;
    }
    exports2.isSet = isSet;
    function isWeakMapToString(value) {
      return ObjectToString(value) === "[object WeakMap]";
    }
    isWeakMapToString.working = typeof WeakMap !== "undefined" && isWeakMapToString(/* @__PURE__ */ new WeakMap());
    function isWeakMap(value) {
      if (typeof WeakMap === "undefined") {
        return false;
      }
      return isWeakMapToString.working ? isWeakMapToString(value) : value instanceof WeakMap;
    }
    exports2.isWeakMap = isWeakMap;
    function isWeakSetToString(value) {
      return ObjectToString(value) === "[object WeakSet]";
    }
    isWeakSetToString.working = typeof WeakSet !== "undefined" && isWeakSetToString(/* @__PURE__ */ new WeakSet());
    function isWeakSet(value) {
      return isWeakSetToString(value);
    }
    exports2.isWeakSet = isWeakSet;
    function isArrayBufferToString(value) {
      return ObjectToString(value) === "[object ArrayBuffer]";
    }
    isArrayBufferToString.working = typeof ArrayBuffer !== "undefined" && isArrayBufferToString(new ArrayBuffer());
    function isArrayBuffer(value) {
      if (typeof ArrayBuffer === "undefined") {
        return false;
      }
      return isArrayBufferToString.working ? isArrayBufferToString(value) : value instanceof ArrayBuffer;
    }
    exports2.isArrayBuffer = isArrayBuffer;
    function isDataViewToString(value) {
      return ObjectToString(value) === "[object DataView]";
    }
    isDataViewToString.working = typeof ArrayBuffer !== "undefined" && typeof DataView !== "undefined" && isDataViewToString(new DataView(new ArrayBuffer(1), 0, 1));
    function isDataView(value) {
      if (typeof DataView === "undefined") {
        return false;
      }
      return isDataViewToString.working ? isDataViewToString(value) : value instanceof DataView;
    }
    exports2.isDataView = isDataView;
    var SharedArrayBufferCopy = typeof SharedArrayBuffer !== "undefined" ? SharedArrayBuffer : void 0;
    function isSharedArrayBufferToString(value) {
      return ObjectToString(value) === "[object SharedArrayBuffer]";
    }
    function isSharedArrayBuffer(value) {
      if (typeof SharedArrayBufferCopy === "undefined") {
        return false;
      }
      if (typeof isSharedArrayBufferToString.working === "undefined") {
        isSharedArrayBufferToString.working = isSharedArrayBufferToString(new SharedArrayBufferCopy());
      }
      return isSharedArrayBufferToString.working ? isSharedArrayBufferToString(value) : value instanceof SharedArrayBufferCopy;
    }
    exports2.isSharedArrayBuffer = isSharedArrayBuffer;
    function isAsyncFunction(value) {
      return ObjectToString(value) === "[object AsyncFunction]";
    }
    exports2.isAsyncFunction = isAsyncFunction;
    function isMapIterator(value) {
      return ObjectToString(value) === "[object Map Iterator]";
    }
    exports2.isMapIterator = isMapIterator;
    function isSetIterator(value) {
      return ObjectToString(value) === "[object Set Iterator]";
    }
    exports2.isSetIterator = isSetIterator;
    function isGeneratorObject(value) {
      return ObjectToString(value) === "[object Generator]";
    }
    exports2.isGeneratorObject = isGeneratorObject;
    function isWebAssemblyCompiledModule(value) {
      return ObjectToString(value) === "[object WebAssembly.Module]";
    }
    exports2.isWebAssemblyCompiledModule = isWebAssemblyCompiledModule;
    function isNumberObject(value) {
      return checkBoxedPrimitive(value, numberValue);
    }
    exports2.isNumberObject = isNumberObject;
    function isStringObject(value) {
      return checkBoxedPrimitive(value, stringValue);
    }
    exports2.isStringObject = isStringObject;
    function isBooleanObject(value) {
      return checkBoxedPrimitive(value, booleanValue);
    }
    exports2.isBooleanObject = isBooleanObject;
    function isBigIntObject(value) {
      return BigIntSupported && checkBoxedPrimitive(value, bigIntValue);
    }
    exports2.isBigIntObject = isBigIntObject;
    function isSymbolObject(value) {
      return SymbolSupported && checkBoxedPrimitive(value, symbolValue);
    }
    exports2.isSymbolObject = isSymbolObject;
    function isBoxedPrimitive(value) {
      return isNumberObject(value) || isStringObject(value) || isBooleanObject(value) || isBigIntObject(value) || isSymbolObject(value);
    }
    exports2.isBoxedPrimitive = isBoxedPrimitive;
    function isAnyArrayBuffer(value) {
      return typeof Uint8Array !== "undefined" && (isArrayBuffer(value) || isSharedArrayBuffer(value));
    }
    exports2.isAnyArrayBuffer = isAnyArrayBuffer;
    ["isProxy", "isExternal", "isModuleNamespaceObject"].forEach(function(method) {
      Object.defineProperty(exports2, method, {
        enumerable: false,
        value: function() {
          throw new Error(method + " is not supported in userland");
        }
      });
    });
  }
});

// node_modules/.pnpm/util@0.12.5/node_modules/util/support/isBufferBrowser.js
var require_isBufferBrowser = __commonJS({
  "node_modules/.pnpm/util@0.12.5/node_modules/util/support/isBufferBrowser.js"(exports2, module2) {
    module2.exports = function isBuffer(arg) {
      return arg && typeof arg === "object" && typeof arg.copy === "function" && typeof arg.fill === "function" && typeof arg.readUInt8 === "function";
    };
  }
});

// node_modules/.pnpm/inherits@2.0.4/node_modules/inherits/inherits_browser.js
var require_inherits_browser = __commonJS({
  "node_modules/.pnpm/inherits@2.0.4/node_modules/inherits/inherits_browser.js"(exports2, module2) {
    if (typeof Object.create === "function") {
      module2.exports = function inherits(ctor, superCtor) {
        if (superCtor) {
          ctor.super_ = superCtor;
          ctor.prototype = Object.create(superCtor.prototype, {
            constructor: {
              value: ctor,
              enumerable: false,
              writable: true,
              configurable: true
            }
          });
        }
      };
    } else {
      module2.exports = function inherits(ctor, superCtor) {
        if (superCtor) {
          ctor.super_ = superCtor;
          var TempCtor = function() {
          };
          TempCtor.prototype = superCtor.prototype;
          ctor.prototype = new TempCtor();
          ctor.prototype.constructor = ctor;
        }
      };
    }
  }
});

// node_modules/.pnpm/util@0.12.5/node_modules/util/util.js
var require_util = __commonJS({
  "node_modules/.pnpm/util@0.12.5/node_modules/util/util.js"(exports2) {
    var getOwnPropertyDescriptors = Object.getOwnPropertyDescriptors || function getOwnPropertyDescriptors2(obj) {
      var keys = Object.keys(obj);
      var descriptors = {};
      for (var i = 0; i < keys.length; i++) {
        descriptors[keys[i]] = Object.getOwnPropertyDescriptor(obj, keys[i]);
      }
      return descriptors;
    };
    var formatRegExp = /%[sdj%]/g;
    exports2.format = function(f) {
      if (!isString(f)) {
        var objects = [];
        for (var i = 0; i < arguments.length; i++) {
          objects.push(inspect(arguments[i]));
        }
        return objects.join(" ");
      }
      var i = 1;
      var args = arguments;
      var len = args.length;
      var str = String(f).replace(formatRegExp, function(x2) {
        if (x2 === "%%") return "%";
        if (i >= len) return x2;
        switch (x2) {
          case "%s":
            return String(args[i++]);
          case "%d":
            return Number(args[i++]);
          case "%j":
            try {
              return JSON.stringify(args[i++]);
            } catch (_) {
              return "[Circular]";
            }
          default:
            return x2;
        }
      });
      for (var x = args[i]; i < len; x = args[++i]) {
        if (isNull(x) || !isObject(x)) {
          str += " " + x;
        } else {
          str += " " + inspect(x);
        }
      }
      return str;
    };
    exports2.deprecate = function(fn, msg) {
      if (typeof process !== "undefined" && process.noDeprecation === true) {
        return fn;
      }
      if (typeof process === "undefined") {
        return function() {
          return exports2.deprecate(fn, msg).apply(this, arguments);
        };
      }
      var warned = false;
      function deprecated() {
        if (!warned) {
          if (process.throwDeprecation) {
            throw new Error(msg);
          } else if (process.traceDeprecation) {
            console.trace(msg);
          } else {
            console.error(msg);
          }
          warned = true;
        }
        return fn.apply(this, arguments);
      }
      return deprecated;
    };
    var debugs = {};
    var debugEnvRegex = /^$/;
    if (process.env.NODE_DEBUG) {
      debugEnv = process.env.NODE_DEBUG;
      debugEnv = debugEnv.replace(/[|\\\\{}()[\\]^$+?.]/g, "\\\\$&").replace(/\\*/g, ".*").replace(/,/g, "$|^").toUpperCase();
      debugEnvRegex = new RegExp("^" + debugEnv + "$", "i");
    }
    var debugEnv;
    exports2.debuglog = function(set) {
      set = set.toUpperCase();
      if (!debugs[set]) {
        if (debugEnvRegex.test(set)) {
          var pid = process.pid;
          debugs[set] = function() {
            var msg = exports2.format.apply(exports2, arguments);
            console.error("%s %d: %s", set, pid, msg);
          };
        } else {
          debugs[set] = function() {
          };
        }
      }
      return debugs[set];
    };
    function inspect(obj, opts) {
      var ctx = {
        seen: [],
        stylize: stylizeNoColor
      };
      if (arguments.length >= 3) ctx.depth = arguments[2];
      if (arguments.length >= 4) ctx.colors = arguments[3];
      if (isBoolean(opts)) {
        ctx.showHidden = opts;
      } else if (opts) {
        exports2._extend(ctx, opts);
      }
      if (isUndefined(ctx.showHidden)) ctx.showHidden = false;
      if (isUndefined(ctx.depth)) ctx.depth = 2;
      if (isUndefined(ctx.colors)) ctx.colors = false;
      if (isUndefined(ctx.customInspect)) ctx.customInspect = true;
      if (ctx.colors) ctx.stylize = stylizeWithColor;
      return formatValue(ctx, obj, ctx.depth);
    }
    exports2.inspect = inspect;
    inspect.colors = {
      "bold": [1, 22],
      "italic": [3, 23],
      "underline": [4, 24],
      "inverse": [7, 27],
      "white": [37, 39],
      "grey": [90, 39],
      "black": [30, 39],
      "blue": [34, 39],
      "cyan": [36, 39],
      "green": [32, 39],
      "magenta": [35, 39],
      "red": [31, 39],
      "yellow": [33, 39]
    };
    inspect.styles = {
      "special": "cyan",
      "number": "yellow",
      "boolean": "yellow",
      "undefined": "grey",
      "null": "bold",
      "string": "green",
      "date": "magenta",
      // "name": intentionally not styling
      "regexp": "red"
    };
    function stylizeWithColor(str, styleType) {
      var style = inspect.styles[styleType];
      if (style) {
        return "\\x1B[" + inspect.colors[style][0] + "m" + str + "\\x1B[" + inspect.colors[style][1] + "m";
      } else {
        return str;
      }
    }
    function stylizeNoColor(str, styleType) {
      return str;
    }
    function arrayToHash(array) {
      var hash = {};
      array.forEach(function(val, idx) {
        hash[val] = true;
      });
      return hash;
    }
    function formatValue(ctx, value, recurseTimes) {
      if (ctx.customInspect && value && isFunction(value.inspect) && // Filter out the util module, it's inspect function is special
      value.inspect !== exports2.inspect && // Also filter out any prototype objects using the circular check.
      !(value.constructor && value.constructor.prototype === value)) {
        var ret = value.inspect(recurseTimes, ctx);
        if (!isString(ret)) {
          ret = formatValue(ctx, ret, recurseTimes);
        }
        return ret;
      }
      var primitive = formatPrimitive(ctx, value);
      if (primitive) {
        return primitive;
      }
      var keys = Object.keys(value);
      var visibleKeys = arrayToHash(keys);
      if (ctx.showHidden) {
        keys = Object.getOwnPropertyNames(value);
      }
      if (isError(value) && (keys.indexOf("message") >= 0 || keys.indexOf("description") >= 0)) {
        return formatError(value);
      }
      if (keys.length === 0) {
        if (isFunction(value)) {
          var name = value.name ? ": " + value.name : "";
          return ctx.stylize("[Function" + name + "]", "special");
        }
        if (isRegExp(value)) {
          return ctx.stylize(RegExp.prototype.toString.call(value), "regexp");
        }
        if (isDate(value)) {
          return ctx.stylize(Date.prototype.toString.call(value), "date");
        }
        if (isError(value)) {
          return formatError(value);
        }
      }
      var base = "", array = false, braces = ["{", "}"];
      if (isArray(value)) {
        array = true;
        braces = ["[", "]"];
      }
      if (isFunction(value)) {
        var n = value.name ? ": " + value.name : "";
        base = " [Function" + n + "]";
      }
      if (isRegExp(value)) {
        base = " " + RegExp.prototype.toString.call(value);
      }
      if (isDate(value)) {
        base = " " + Date.prototype.toUTCString.call(value);
      }
      if (isError(value)) {
        base = " " + formatError(value);
      }
      if (keys.length === 0 && (!array || value.length == 0)) {
        return braces[0] + base + braces[1];
      }
      if (recurseTimes < 0) {
        if (isRegExp(value)) {
          return ctx.stylize(RegExp.prototype.toString.call(value), "regexp");
        } else {
          return ctx.stylize("[Object]", "special");
        }
      }
      ctx.seen.push(value);
      var output;
      if (array) {
        output = formatArray(ctx, value, recurseTimes, visibleKeys, keys);
      } else {
        output = keys.map(function(key) {
          return formatProperty(ctx, value, recurseTimes, visibleKeys, key, array);
        });
      }
      ctx.seen.pop();
      return reduceToSingleString(output, base, braces);
    }
    function formatPrimitive(ctx, value) {
      if (isUndefined(value))
        return ctx.stylize("undefined", "undefined");
      if (isString(value)) {
        var simple = "'" + JSON.stringify(value).replace(/^"|"$/g, "").replace(/'/g, "\\\\'").replace(/\\\\"/g, '"') + "'";
        return ctx.stylize(simple, "string");
      }
      if (isNumber(value))
        return ctx.stylize("" + value, "number");
      if (isBoolean(value))
        return ctx.stylize("" + value, "boolean");
      if (isNull(value))
        return ctx.stylize("null", "null");
    }
    function formatError(value) {
      return "[" + Error.prototype.toString.call(value) + "]";
    }
    function formatArray(ctx, value, recurseTimes, visibleKeys, keys) {
      var output = [];
      for (var i = 0, l = value.length; i < l; ++i) {
        if (hasOwnProperty(value, String(i))) {
          output.push(formatProperty(
            ctx,
            value,
            recurseTimes,
            visibleKeys,
            String(i),
            true
          ));
        } else {
          output.push("");
        }
      }
      keys.forEach(function(key) {
        if (!key.match(/^\\d+$/)) {
          output.push(formatProperty(
            ctx,
            value,
            recurseTimes,
            visibleKeys,
            key,
            true
          ));
        }
      });
      return output;
    }
    function formatProperty(ctx, value, recurseTimes, visibleKeys, key, array) {
      var name, str, desc;
      desc = Object.getOwnPropertyDescriptor(value, key) || { value: value[key] };
      if (desc.get) {
        if (desc.set) {
          str = ctx.stylize("[Getter/Setter]", "special");
        } else {
          str = ctx.stylize("[Getter]", "special");
        }
      } else {
        if (desc.set) {
          str = ctx.stylize("[Setter]", "special");
        }
      }
      if (!hasOwnProperty(visibleKeys, key)) {
        name = "[" + key + "]";
      }
      if (!str) {
        if (ctx.seen.indexOf(desc.value) < 0) {
          if (isNull(recurseTimes)) {
            str = formatValue(ctx, desc.value, null);
          } else {
            str = formatValue(ctx, desc.value, recurseTimes - 1);
          }
          if (str.indexOf("\\n") > -1) {
            if (array) {
              str = str.split("\\n").map(function(line) {
                return "  " + line;
              }).join("\\n").slice(2);
            } else {
              str = "\\n" + str.split("\\n").map(function(line) {
                return "   " + line;
              }).join("\\n");
            }
          }
        } else {
          str = ctx.stylize("[Circular]", "special");
        }
      }
      if (isUndefined(name)) {
        if (array && key.match(/^\\d+$/)) {
          return str;
        }
        name = JSON.stringify("" + key);
        if (name.match(/^"([a-zA-Z_][a-zA-Z_0-9]*)"$/)) {
          name = name.slice(1, -1);
          name = ctx.stylize(name, "name");
        } else {
          name = name.replace(/'/g, "\\\\'").replace(/\\\\"/g, '"').replace(/(^"|"$)/g, "'");
          name = ctx.stylize(name, "string");
        }
      }
      return name + ": " + str;
    }
    function reduceToSingleString(output, base, braces) {
      var numLinesEst = 0;
      var length = output.reduce(function(prev, cur) {
        numLinesEst++;
        if (cur.indexOf("\\n") >= 0) numLinesEst++;
        return prev + cur.replace(/\\u001b\\[\\d\\d?m/g, "").length + 1;
      }, 0);
      if (length > 60) {
        return braces[0] + (base === "" ? "" : base + "\\n ") + " " + output.join(",\\n  ") + " " + braces[1];
      }
      return braces[0] + base + " " + output.join(", ") + " " + braces[1];
    }
    exports2.types = require_types();
    function isArray(ar) {
      return Array.isArray(ar);
    }
    exports2.isArray = isArray;
    function isBoolean(arg) {
      return typeof arg === "boolean";
    }
    exports2.isBoolean = isBoolean;
    function isNull(arg) {
      return arg === null;
    }
    exports2.isNull = isNull;
    function isNullOrUndefined(arg) {
      return arg == null;
    }
    exports2.isNullOrUndefined = isNullOrUndefined;
    function isNumber(arg) {
      return typeof arg === "number";
    }
    exports2.isNumber = isNumber;
    function isString(arg) {
      return typeof arg === "string";
    }
    exports2.isString = isString;
    function isSymbol(arg) {
      return typeof arg === "symbol";
    }
    exports2.isSymbol = isSymbol;
    function isUndefined(arg) {
      return arg === void 0;
    }
    exports2.isUndefined = isUndefined;
    function isRegExp(re) {
      return isObject(re) && objectToString(re) === "[object RegExp]";
    }
    exports2.isRegExp = isRegExp;
    exports2.types.isRegExp = isRegExp;
    function isObject(arg) {
      return typeof arg === "object" && arg !== null;
    }
    exports2.isObject = isObject;
    function isDate(d) {
      return isObject(d) && objectToString(d) === "[object Date]";
    }
    exports2.isDate = isDate;
    exports2.types.isDate = isDate;
    function isError(e) {
      return isObject(e) && (objectToString(e) === "[object Error]" || e instanceof Error);
    }
    exports2.isError = isError;
    exports2.types.isNativeError = isError;
    function isFunction(arg) {
      return typeof arg === "function";
    }
    exports2.isFunction = isFunction;
    function isPrimitive(arg) {
      return arg === null || typeof arg === "boolean" || typeof arg === "number" || typeof arg === "string" || typeof arg === "symbol" || // ES6 symbol
      typeof arg === "undefined";
    }
    exports2.isPrimitive = isPrimitive;
    exports2.isBuffer = require_isBufferBrowser();
    function objectToString(o) {
      return Object.prototype.toString.call(o);
    }
    function pad(n) {
      return n < 10 ? "0" + n.toString(10) : n.toString(10);
    }
    var months = [
      "Jan",
      "Feb",
      "Mar",
      "Apr",
      "May",
      "Jun",
      "Jul",
      "Aug",
      "Sep",
      "Oct",
      "Nov",
      "Dec"
    ];
    function timestamp() {
      var d = /* @__PURE__ */ new Date();
      var time = [
        pad(d.getHours()),
        pad(d.getMinutes()),
        pad(d.getSeconds())
      ].join(":");
      return [d.getDate(), months[d.getMonth()], time].join(" ");
    }
    exports2.log = function() {
      console.log("%s - %s", timestamp(), exports2.format.apply(exports2, arguments));
    };
    exports2.inherits = require_inherits_browser();
    exports2._extend = function(origin, add) {
      if (!add || !isObject(add)) return origin;
      var keys = Object.keys(add);
      var i = keys.length;
      while (i--) {
        origin[keys[i]] = add[keys[i]];
      }
      return origin;
    };
    function hasOwnProperty(obj, prop) {
      return Object.prototype.hasOwnProperty.call(obj, prop);
    }
    var kCustomPromisifiedSymbol = typeof Symbol !== "undefined" ? /* @__PURE__ */ Symbol("util.promisify.custom") : void 0;
    exports2.promisify = function promisify(original) {
      if (typeof original !== "function")
        throw new TypeError('The "original" argument must be of type Function');
      if (kCustomPromisifiedSymbol && original[kCustomPromisifiedSymbol]) {
        var fn = original[kCustomPromisifiedSymbol];
        if (typeof fn !== "function") {
          throw new TypeError('The "util.promisify.custom" argument must be of type Function');
        }
        Object.defineProperty(fn, kCustomPromisifiedSymbol, {
          value: fn,
          enumerable: false,
          writable: false,
          configurable: true
        });
        return fn;
      }
      function fn() {
        var promiseResolve, promiseReject;
        var promise = new Promise(function(resolve, reject) {
          promiseResolve = resolve;
          promiseReject = reject;
        });
        var args = [];
        for (var i = 0; i < arguments.length; i++) {
          args.push(arguments[i]);
        }
        args.push(function(err, value) {
          if (err) {
            promiseReject(err);
          } else {
            promiseResolve(value);
          }
        });
        try {
          original.apply(this, args);
        } catch (err) {
          promiseReject(err);
        }
        return promise;
      }
      Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
      if (kCustomPromisifiedSymbol) Object.defineProperty(fn, kCustomPromisifiedSymbol, {
        value: fn,
        enumerable: false,
        writable: false,
        configurable: true
      });
      return Object.defineProperties(
        fn,
        getOwnPropertyDescriptors(original)
      );
    };
    exports2.promisify.custom = kCustomPromisifiedSymbol;
    function callbackifyOnRejected(reason, cb) {
      if (!reason) {
        var newReason = new Error("Promise was rejected with a falsy value");
        newReason.reason = reason;
        reason = newReason;
      }
      return cb(reason);
    }
    function callbackify(original) {
      if (typeof original !== "function") {
        throw new TypeError('The "original" argument must be of type Function');
      }
      function callbackified() {
        var args = [];
        for (var i = 0; i < arguments.length; i++) {
          args.push(arguments[i]);
        }
        var maybeCb = args.pop();
        if (typeof maybeCb !== "function") {
          throw new TypeError("The last argument must be of type Function");
        }
        var self = this;
        var cb = function() {
          return maybeCb.apply(self, arguments);
        };
        original.apply(this, args).then(
          function(ret) {
            process.nextTick(cb.bind(null, null, ret));
          },
          function(rej) {
            process.nextTick(callbackifyOnRejected.bind(null, rej, cb));
          }
        );
      }
      Object.setPrototypeOf(callbackified, Object.getPrototypeOf(original));
      Object.defineProperties(
        callbackified,
        getOwnPropertyDescriptors(original)
      );
      return callbackified;
    }
    exports2.callbackify = callbackify;
  }
});

// <stdin>
var util = require_util();
module.exports = util.default ?? util;

function installBuiltinUtilFormatWithOptions(builtinUtilModule) {
    if (!builtinUtilModule || typeof builtinUtilModule.formatWithOptions === "function") {
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
module.exports = installBuiltinUtilFormatWithOptions(module.exports);
if (module.exports && module.exports.default == null) module.exports.default = module.exports;
`;
  }
});

// ../../../agent-os/packages/browser/dist/runtime.js
var POLYFILL_CODE_MAP;
var init_runtime = __esm({
  "../../../agent-os/packages/browser/dist/runtime.js"() {
    "use strict";
    init_os_filesystem();
    init_encoding();
    init_wasi_polyfill();
    init_signals();
    init_buffer_polyfill();
    init_path_polyfill();
    init_util_polyfill();
    POLYFILL_CODE_MAP = {
      fs: "module.exports = globalThis._fsModule;",
      "node:fs": "module.exports = globalThis._fsModule;",
      "fs/promises": "module.exports = globalThis._fsModule.promises || globalThis._fsModule;",
      "node:fs/promises": "module.exports = globalThis._fsModule.promises || globalThis._fsModule;",
      util: BROWSER_UTIL_POLYFILL_CODE,
      "node:util": "module.exports = require('util');",
      "util/types": "module.exports = require('util').types;",
      "node:util/types": "module.exports = require('util/types');",
      buffer: BROWSER_BUFFER_POLYFILL_CODE,
      "node:buffer": "module.exports = require('buffer');",
      path: BROWSER_PATH_POLYFILL_CODE,
      "node:path": "module.exports = require('path');",
      console: "module.exports = globalThis.console;",
      "node:console": "module.exports = require('console');",
      process: "module.exports = globalThis.process;",
      "node:process": "module.exports = globalThis.process;",
      // node:module — createRequire returns the guest's kernel-backed require so guest
      // programs (e.g. the pi ACP adapter) can build a require from import.meta.url.
      module: `
		const createRequire = () => globalThis.require;
		const Module = { createRequire };
		module.exports = { createRequire, Module, builtinModules: [] };
		module.exports.default = module.exports;
	`,
      "node:module": "module.exports = require('module');",
      // node:stream — a minimal but functional stream set. The ACP connection itself
      // uses WHATWG Readable/WritableStream (worker globals); guest programs use these
      // node streams for buffering (e.g. pi's bufferedStdin PassThrough). Readable.toWeb
      // / Writable.toWeb bridge to the WHATWG streams the ACP codec consumes.
      stream: `
		class EventEmitterLike {
			constructor() { this._listeners = Object.create(null); }
			on(event, fn) { (this._listeners[event] = this._listeners[event] || []).push(fn); return this; }
			addListener(event, fn) { return this.on(event, fn); }
			once(event, fn) { const w = (...a) => { this.off(event, w); fn(...a); }; w._origin = fn; return this.on(event, w); }
			off(event, fn) { if (this._listeners[event]) this._listeners[event] = this._listeners[event].filter((x) => x !== fn && x._origin !== fn); return this; }
			removeListener(event, fn) { return this.off(event, fn); }
			removeAllListeners(event) { if (event) delete this._listeners[event]; else this._listeners = Object.create(null); return this; }
			emit(event, ...args) { const ls = (this._listeners[event] || []).slice(); for (const fn of ls) fn(...args); return ls.length > 0; }
			listenerCount(event) { return (this._listeners[event] || []).length; }
		}
		class Readable extends EventEmitterLike {
			constructor(options) { super(); this.readable = true; this._readableOptions = options || {}; if (this._readableOptions.read) this._read = this._readableOptions.read; }
			resume() { this.emit("resume"); return this; }
			pause() { this.paused = true; return this; }
			setEncoding() { return this; }
			read() { return null; }
			push(chunk) { if (chunk == null) this.emit("end"); else this.emit("data", chunk); return true; }
			pipe(dest) { this.on("data", (c) => dest.write && dest.write(c)); this.on("end", () => dest.end && dest.end()); return dest; }
			destroy() { this.emit("close"); return this; }
		}
		Readable.toWeb = (stream) => new ReadableStream({ start(controller) {
			stream.on("data", (chunk) => controller.enqueue(chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk)));
			stream.on("end", () => { try { controller.close(); } catch (e) {} });
			stream.on("error", (err) => controller.error(err));
		} });
		class Writable extends EventEmitterLike {
			constructor(options) { super(); this.writable = true; this._writableOptions = options || {}; if (this._writableOptions.write) this._writeImpl = this._writableOptions.write; }
			write(chunk, encoding, cb) { if (typeof encoding === "function") { cb = encoding; encoding = undefined; } if (this._writeImpl) this._writeImpl(chunk, encoding, cb || (() => {})); else if (cb) cb(); this.emit("data", chunk); return true; }
			end(chunk, encoding, cb) { const done = typeof chunk === "function" ? chunk : typeof encoding === "function" ? encoding : cb; if (chunk != null && typeof chunk !== "function") this.write(chunk); this.emit("finish"); this.emit("end"); if (done) done(); }
			destroy() { this.emit("close"); return this; }
		}
		Writable.toWeb = (stream) => new WritableStream({ write(chunk) { return new Promise((resolve) => stream.write(chunk, undefined, () => resolve())); }, close() { stream.end && stream.end(); } });
		class Duplex extends Readable { constructor(options) { super(options); this.writable = true; if (options && options.write) this._writeImpl = options.write; } write(chunk, encoding, cb) { if (typeof encoding === "function") { cb = encoding; } if (this._writeImpl) this._writeImpl(chunk, encoding, cb || (() => {})); else if (cb) cb(); return true; } end(chunk) { if (chunk != null) this.write(chunk); this.emit("finish"); this.emit("end"); } }
		class Transform extends Duplex {}
		class PassThrough extends Transform { write(chunk, encoding, cb) { if (typeof encoding === "function") { cb = encoding; } this.emit("data", chunk); if (cb) cb(); return true; } end(chunk) { if (chunk != null) this.emit("data", chunk); this.emit("end"); this.emit("finish"); } }
		function finished(stream, optsOrCb, maybeCb) {
			const cb = typeof optsOrCb === "function" ? optsOrCb : maybeCb;
			if (stream && stream.on) { let done = false; const fire = (e) => { if (done) return; done = true; if (cb) cb(e || null); }; stream.on("end", () => fire()); stream.on("finish", () => fire()); stream.on("close", () => fire()); stream.on("error", (e) => fire(e)); }
			return () => {};
		}
		function pipeline(...args) {
			const cb = typeof args[args.length - 1] === "function" ? args.pop() : null;
			const streams = args.flat();
			for (let i = 0; i < streams.length - 1; i++) { if (streams[i] && streams[i].pipe) streams[i].pipe(streams[i + 1]); }
			const last = streams[streams.length - 1];
			if (last && last.on) { last.on("finish", () => cb && cb(null)); last.on("end", () => cb && cb(null)); last.on("error", (e) => cb && cb(e)); }
			return last;
		}
		const Stream = EventEmitterLike;
		Stream.Readable = Readable; Stream.Writable = Writable; Stream.Duplex = Duplex; Stream.Transform = Transform; Stream.PassThrough = PassThrough;
		module.exports = { Stream, Readable, Writable, Duplex, Transform, PassThrough, finished, pipeline };
		module.exports.promises = { finished: (s) => new Promise((res, rej) => finished(s, (e) => (e ? rej(e) : res()))), pipeline: (...a) => new Promise((res, rej) => pipeline(...a, (e) => (e ? rej(e) : res()))) };
		module.exports.default = module.exports;
	`,
      "node:stream": "module.exports = require('stream');",
      "stream/promises": "module.exports = require('stream').promises;",
      "node:stream/promises": "module.exports = require('stream').promises;",
      "stream/web": "module.exports = { ReadableStream: globalThis.ReadableStream, WritableStream: globalThis.WritableStream, TransformStream: globalThis.TransformStream };",
      "node:stream/web": "module.exports = require('stream/web');",
      // node:constants — fs/os constant values guest programs reference (open flags, etc.).
      constants: `
		module.exports = {
			O_RDONLY: 0, O_WRONLY: 1, O_RDWR: 2, O_CREAT: 64, O_EXCL: 128, O_NOCTTY: 256,
			O_TRUNC: 512, O_APPEND: 1024, O_DIRECTORY: 65536, O_NOFOLLOW: 131072, O_SYNC: 1052672,
			O_NONBLOCK: 2048, S_IFMT: 61440, S_IFREG: 32768, S_IFDIR: 16384, S_IFCHR: 8192,
			S_IFLNK: 40960, S_IFIFO: 4096, S_IFSOCK: 49152, F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1,
			COPYFILE_EXCL: 1, SIGINT: 2, SIGTERM: 15, SIGKILL: 9, SIGHUP: 1,
		};
		module.exports.default = module.exports;
	`,
      "node:constants": "module.exports = require('constants');",
      // node:events — EventEmitter (a complete-enough implementation for guest libraries).
      events: `
		class EventEmitter {
			constructor() { this._events = Object.create(null); this._max = 10; }
			setMaxListeners(n) { this._max = n; return this; }
			getMaxListeners() { return this._max; }
			on(type, fn) { (this._events[type] = this._events[type] || []).push(fn); this.emit("newListener", type, fn); return this; }
			addListener(type, fn) { return this.on(type, fn); }
			prependListener(type, fn) { (this._events[type] = this._events[type] || []).unshift(fn); return this; }
			once(type, fn) { const w = (...a) => { this.off(type, w); fn(...a); }; w.listener = fn; return this.on(type, w); }
			prependOnceListener(type, fn) { const w = (...a) => { this.off(type, w); fn(...a); }; w.listener = fn; return this.prependListener(type, w); }
			off(type, fn) { const l = this._events[type]; if (l) { this._events[type] = l.filter((x) => x !== fn && x.listener !== fn); if (this._events[type].length === 0) delete this._events[type]; } return this; }
			removeListener(type, fn) { return this.off(type, fn); }
			removeAllListeners(type) { if (type) delete this._events[type]; else this._events = Object.create(null); return this; }
			emit(type, ...args) { const l = this._events[type]; if (!l || l.length === 0) { if (type === "error") throw args[0] instanceof Error ? args[0] : new Error("Unhandled error"); return false; } for (const fn of l.slice()) fn.apply(this, args); return true; }
			listeners(type) { return (this._events[type] || []).slice(); }
			rawListeners(type) { return (this._events[type] || []).slice(); }
			listenerCount(type) { return (this._events[type] || []).length; }
			eventNames() { return Object.keys(this._events); }
		}
		EventEmitter.EventEmitter = EventEmitter;
		EventEmitter.once = (emitter, name) => new Promise((resolve, reject) => {
			const ok = (...a) => { emitter.off("error", err); resolve(a); };
			const err = (e) => { emitter.off(name, ok); reject(e); };
			emitter.once(name, ok); emitter.once("error", err);
		});
		EventEmitter.defaultMaxListeners = 10;
		module.exports = EventEmitter;
		module.exports.default = EventEmitter;
	`,
      "node:events": "module.exports = require('events');",
      // node:assert — the common assertion surface.
      assert: `
		function AssertionError(message) { const e = new Error(message); e.name = "AssertionError"; return e; }
		function assert(value, message) { if (!value) throw AssertionError(message || "assertion failed"); }
		assert.ok = assert;
		assert.equal = (a, b, m) => { if (a != b) throw AssertionError(m || (a + " != " + b)); };
		assert.strictEqual = (a, b, m) => { if (a !== b) throw AssertionError(m || (a + " !== " + b)); };
		assert.notEqual = (a, b, m) => { if (a == b) throw AssertionError(m); };
		assert.notStrictEqual = (a, b, m) => { if (a === b) throw AssertionError(m); };
		assert.deepEqual = (a, b, m) => { if (JSON.stringify(a) !== JSON.stringify(b)) throw AssertionError(m); };
		assert.deepStrictEqual = assert.deepEqual;
		assert.fail = (m) => { throw AssertionError(m || "failed"); };
		assert.throws = (fn, m) => { try { fn(); } catch (e) { return; } throw AssertionError(m || "missing expected exception"); };
		assert.AssertionError = AssertionError;
		module.exports = assert;
		module.exports.default = assert;
	`,
      "node:assert": "module.exports = require('assert');",
      // node:url — WHATWG URL globals + the legacy parse/format surface.
      url: `
		module.exports = {
			URL: globalThis.URL,
			URLSearchParams: globalThis.URLSearchParams,
			parse(input) { try { const u = new URL(input); return { href: u.href, protocol: u.protocol, host: u.host, hostname: u.hostname, port: u.port, pathname: u.pathname, search: u.search, hash: u.hash, query: u.search.replace(/^\\?/, ""), path: u.pathname + u.search }; } catch (e) { return { href: input, pathname: input }; } },
			format(u) { if (typeof u === "string") return u; const proto = u.protocol ? (u.protocol.endsWith(":") ? u.protocol : u.protocol + ":") : ""; return proto + "//" + (u.host || u.hostname || "") + (u.pathname || "") + (u.search || (u.query ? "?" + u.query : "")) + (u.hash || ""); },
			resolve(from, to) { try { return new URL(to, from).href; } catch (e) { return to; } },
			fileURLToPath(u) { const s = typeof u === "string" ? u : u.href; return s.replace(/^file:\\/\\//, ""); },
			pathToFileURL(p) { return new URL("file://" + (p.startsWith("/") ? p : "/" + p)); },
			domainToASCII: (d) => d,
			domainToUnicode: (d) => d,
		};
		module.exports.default = module.exports;
	`,
      "node:url": "module.exports = require('url');",
      // node:string_decoder — UTF-8 incremental decoder (TextDecoder-backed).
      string_decoder: `
		class StringDecoder {
			constructor(encoding) { this.encoding = encoding || "utf8"; this._decoder = new TextDecoder(this.encoding === "utf8" ? "utf-8" : this.encoding); }
			write(buf) { const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf); return this._decoder.decode(bytes, { stream: true }); }
			end(buf) { const head = buf ? this.write(buf) : ""; return head + this._decoder.decode(); }
		}
		module.exports = { StringDecoder };
		module.exports.default = module.exports;
	`,
      "node:string_decoder": "module.exports = require('string_decoder');",
      // node:querystring — legacy query parsing/serialization.
      querystring: `
		module.exports = {
			parse(str) { const out = Object.create(null); if (!str) return out; for (const pair of String(str).split("&")) { if (!pair) continue; const i = pair.indexOf("="); const k = decodeURIComponent(i < 0 ? pair : pair.slice(0, i)); const v = i < 0 ? "" : decodeURIComponent(pair.slice(i + 1)); if (k in out) { if (Array.isArray(out[k])) out[k].push(v); else out[k] = [out[k], v]; } else out[k] = v; } return out; },
			stringify(obj) { if (!obj) return ""; const parts = []; for (const k of Object.keys(obj)) { const v = obj[k]; if (Array.isArray(v)) for (const item of v) parts.push(encodeURIComponent(k) + "=" + encodeURIComponent(item)); else parts.push(encodeURIComponent(k) + "=" + encodeURIComponent(v)); } return parts.join("&"); },
			escape: encodeURIComponent, unescape: decodeURIComponent,
		};
		module.exports.default = module.exports;
	`,
      "node:querystring": "module.exports = require('querystring');",
      // node:tty — reflects ExecOptions.stdioPty for stdio fds.
      tty: `
		const ttyState = () => globalThis.__agentOSTtyState;
		class ReadStream {
			constructor(fd) { this.fd = fd; this.isTTY = !!ttyState()?.isatty?.(fd); }
			setRawMode(mode) { if (this.fd === 0 && globalThis.process?.stdin?.setRawMode) globalThis.process.stdin.setRawMode(mode); return this; }
		}
		class WriteStream {
			constructor(fd) { this.fd = fd; this.isTTY = !!ttyState()?.isatty?.(fd); }
			get columns() { return ttyState()?.columns?.() ?? 80; }
			get rows() { return ttyState()?.rows?.() ?? 24; }
		}
		module.exports = {
			isatty: (fd) => !!ttyState()?.isatty?.(fd),
			ReadStream,
			WriteStream,
		};
		module.exports.default = module.exports;
	`,
      "node:tty": "module.exports = require('tty');",
      // node:readline — stub interface (in ACP mode stdin is the protocol, not a REPL).
      readline: `
		module.exports = {
			createInterface: () => { const rl = { on: () => rl, once: () => rl, off: () => rl, removeListener: () => rl, removeAllListeners: () => rl, emit: () => false, close: () => {}, question: (q, cb) => { if (typeof cb === "function") cb(""); }, prompt: () => {}, write: () => {}, pause: () => rl, resume: () => rl, setPrompt: () => {}, [Symbol.asyncIterator]: async function* () {} }; return rl; },
			clearLine: () => true, clearScreenDown: () => true, cursorTo: () => true, moveCursor: () => true, emitKeypressEvents: () => {},
		};
		module.exports.default = module.exports;
	`,
      "node:readline": "module.exports = require('readline');",
      "readline/promises": "module.exports = require('readline');",
      "node:readline/promises": "module.exports = require('readline');",
      // node:timers — the timer globals.
      timers: `
		module.exports = { setTimeout: globalThis.setTimeout.bind(globalThis), clearTimeout: globalThis.clearTimeout.bind(globalThis), setInterval: globalThis.setInterval.bind(globalThis), clearInterval: globalThis.clearInterval.bind(globalThis), setImmediate: globalThis.setImmediate, clearImmediate: globalThis.clearImmediate };
		module.exports.default = module.exports;
	`,
      "node:timers": "module.exports = require('timers');",
      "timers/promises": `
		module.exports = { setTimeout: (ms, value) => new Promise((r) => globalThis.setTimeout(() => r(value), ms)), setImmediate: (value) => Promise.resolve(value), setInterval: async function* () {} };
		module.exports.default = module.exports;
	`,
      "node:timers/promises": "module.exports = require('timers/promises');",
      // node:diagnostics_channel / node:inspector — no-op observability stubs.
      diagnostics_channel: `
		module.exports = { channel: () => ({ hasSubscribers: false, publish() {}, subscribe() {}, unsubscribe() {} }), hasSubscribers: () => false, subscribe() {}, unsubscribe() {} };
		module.exports.default = module.exports;
	`,
      "node:diagnostics_channel": "module.exports = require('diagnostics_channel');",
      inspector: `module.exports = { open() {}, close() {}, url: () => undefined, Session: class {} }; module.exports.default = module.exports;`,
      "node:inspector": "module.exports = require('inspector');",
      // node:v8 — heap stats + structured serialize (JSON fallback) guest libs may probe.
      v8: `
		module.exports = {
			serialize: (v) => new TextEncoder().encode(JSON.stringify(v)),
			deserialize: (b) => JSON.parse(new TextDecoder().decode(b)),
			getHeapStatistics: () => ({ total_heap_size: 0, used_heap_size: 0, heap_size_limit: 0 }),
			getHeapSpaceStatistics: () => [],
			setFlagsFromString: () => {},
		};
		module.exports.default = module.exports;
	`,
      "node:v8": "module.exports = require('v8');",
      // node:async_hooks — a working single-threaded AsyncLocalStorage (synchronous store
      // stack; context propagation across awaits is best-effort) + no-op AsyncResource.
      async_hooks: `
		class AsyncLocalStorage {
			constructor() { this._stack = []; }
			run(store, fn, ...args) { this._stack.push(store); try { return fn(...args); } finally { this._stack.pop(); } }
			getStore() { return this._stack.length ? this._stack[this._stack.length - 1] : undefined; }
			enterWith(store) { this._stack.push(store); }
			exit(fn, ...args) { const saved = this._stack; this._stack = []; try { return fn(...args); } finally { this._stack = saved; } }
			disable() { this._stack = []; }
		}
		class AsyncResource { constructor() {} runInAsyncScope(fn, thisArg, ...args) { return fn.apply(thisArg, args); } bind(fn) { return fn; } emitDestroy() { return this; } }
		module.exports = { AsyncLocalStorage, AsyncResource, createHook: () => ({ enable() {}, disable() {} }), executionAsyncId: () => 0, triggerAsyncId: () => 0 };
		module.exports.default = module.exports;
	`,
      "node:async_hooks": "module.exports = require('async_hooks');",
      // node:perf_hooks — the performance global + a no-op observer.
      perf_hooks: `
		module.exports = {
			performance: globalThis.performance,
			PerformanceObserver: class { constructor() {} observe() {} disconnect() {} },
			monitorEventLoopDelay: () => ({ enable() {}, disable() {}, reset() {} }),
		};
		module.exports.default = module.exports;
	`,
      "node:perf_hooks": "module.exports = require('perf_hooks');",
      // node:zlib — present but unsupported; throws only if actually used (often imported,
      // not exercised, on the guest happy path).
      zlib: `
		const unsupported = () => { throw new Error("zlib is not supported in the browser runtime"); };
		module.exports = { gzip: unsupported, gunzip: unsupported, gzipSync: unsupported, gunzipSync: unsupported, deflate: unsupported, inflate: unsupported, deflateSync: unsupported, inflateSync: unsupported, brotliCompressSync: unsupported, brotliDecompressSync: unsupported, createGzip: unsupported, createGunzip: unsupported, constants: {} };
		module.exports.default = module.exports;
	`,
      "node:zlib": "module.exports = require('zlib');",
      // node:http / node:https — guest HTTP belongs to global fetch (kernel-brokered);
      // the legacy module surface is a stub that errors only if actually used.
      http: `
		const unsupported = () => { throw new Error("node:http is not supported; use global fetch"); };
		module.exports = { request: unsupported, get: unsupported, createServer: unsupported, Agent: class {}, globalAgent: {}, STATUS_CODES: {}, METHODS: [] };
		module.exports.default = module.exports;
	`,
      "node:http": "module.exports = require('http');",
      https: `module.exports = require('http');`,
      "node:https": "module.exports = require('http');",
      // node:net — stub (kernel sockets are reached via the converged net bridge, not this).
      net: `
		const unsupported = () => { throw new Error("node:net is not supported in this runtime"); };
		module.exports = { connect: unsupported, createConnection: unsupported, createServer: unsupported, Socket: class {}, isIP: () => 0, isIPv4: () => false, isIPv6: () => false };
		module.exports.default = module.exports;
	`,
      "node:net": "module.exports = require('net');",
      // node:vm — minimal: run code in the guest global scope.
      vm: `
		module.exports = {
			runInThisContext: (code) => (0, eval)(code),
			runInNewContext: (code) => (0, eval)(code),
			createContext: (o) => o || {},
			Script: class { constructor(code) { this.code = code; } runInThisContext() { return (0, eval)(this.code); } runInNewContext() { return (0, eval)(this.code); } },
		};
		module.exports.default = module.exports;
	`,
      "node:vm": "module.exports = require('vm');",
      // node:worker_threads — single-threaded: main thread, no spawning.
      worker_threads: `
		module.exports = { isMainThread: true, threadId: 0, parentPort: null, workerData: null, Worker: class { constructor() { throw new Error("worker_threads is not supported in this runtime"); } }, MessageChannel: class {}, MessagePort: class {} };
		module.exports.default = module.exports;
	`,
      "node:worker_threads": "module.exports = require('worker_threads');",
      child_process: `
		const callSync = (ref, ...args) => {
			if (typeof ref === "function") return ref(...args);
			if (ref && typeof ref.applySync === "function") return ref.applySync(undefined, args);
			if (ref && typeof ref.applySyncPromise === "function") return ref.applySyncPromise(undefined, args);
			throw new Error("child_process bridge is not configured");
		};
		const encodeBytes = globalThis.__agentOSEncoding.encodeBytesPayload;
		const decodeBytes = globalThis.__agentOSEncoding.decodeBytesPayload;
		const text = (bytes) => new TextDecoder().decode(bytes);
		const bufferLike = (value) => {
			const bytes = decodeBytes(value);
			bytes.toString = () => text(bytes);
			return bytes;
		};
		class Emitter {
			constructor() {
				this._listeners = new Map();
			}
			on(event, listener) {
				const listeners = this._listeners.get(event) || [];
				listeners.push(listener);
				this._listeners.set(event, listeners);
				return this;
			}
			once(event, listener) {
				const wrapped = (...args) => {
					this.off(event, wrapped);
					listener(...args);
				};
				return this.on(event, wrapped);
			}
			off(event, listener) {
				const listeners = this._listeners.get(event) || [];
				this._listeners.set(event, listeners.filter((entry) => entry !== listener));
				return this;
			}
			removeListener(event, listener) {
				return this.off(event, listener);
			}
			emit(event, ...args) {
				const listeners = this._listeners.get(event) || [];
				for (const listener of [...listeners]) listener(...args);
				return listeners.length > 0;
			}
		}
		class ChildProcess extends Emitter {
			constructor(sessionId) {
				super();
				this.pid = Number(sessionId) || -1;
				this.exitCode = null;
				this.signalCode = null;
				this.killed = false;
				this.stdout = new Emitter();
				this.stderr = new Emitter();
				this.stdin = {
					write: (data) => {
						callSync(globalThis._childProcessStdinWrite, sessionId, typeof data === "string" ? new TextEncoder().encode(data) : data);
						return true;
					},
					end: (data) => {
						if (data != null) this.stdin.write(data);
						callSync(globalThis._childProcessStdinClose, sessionId);
					},
				};
			}
		}
		const normalizeArgs = (args, options) => {
			if (Array.isArray(args)) return { args, options: options || {} };
			return { args: [], options: args || {} };
		};
		const signalNumbers = ${JSON.stringify(PROCESS_SIGNAL_NUMBERS)};
		const normalizeSignal = (signal) => {
			if (signal === undefined || signal === null) return 15;
			if (typeof signal === "number" && Number.isFinite(signal)) {
				const numeric = Math.trunc(signal);
				if (numeric >= 0 && numeric <= 31) return numeric;
				throw unknownSignalError(signal);
			}
			const raw = String(signal).trim();
			if (/^[+-]?\\d+$/.test(raw)) {
				const numeric = Number.parseInt(raw, 10);
				if (numeric >= 0 && numeric <= 31) return numeric;
				throw unknownSignalError(signal);
			}
			const upper = raw.toUpperCase();
			const signalName = upper.startsWith("SIG") ? upper : "SIG" + upper;
			const numeric = signalNumbers[signalName];
			if (numeric !== undefined) return numeric;
			throw unknownSignalError(signal);
		};
		const unknownSignalError = (signal) => {
			const error = new TypeError("Unknown signal: " + String(signal));
			error.code = "ERR_UNKNOWN_SIGNAL";
			return error;
		};
		function spawn(command, argsOrOptions, maybeOptions) {
			const { args, options } = normalizeArgs(argsOrOptions, maybeOptions);
			let sessionId;
			try {
				sessionId = callSync(
					globalThis._childProcessSpawnStart,
					{
						command: String(command),
						args: args.map(String),
						options: {
							cwd: options.cwd || (globalThis.process && globalThis.process.cwd ? globalThis.process.cwd() : "/"),
							env: options.env,
						},
					},
				);
			} catch (error) {
				const child = new ChildProcess(-1);
				queueMicrotask(() => child.emit("error", error));
				return child;
			}
			const child = new ChildProcess(sessionId);
			child.kill = (signal) => {
				callSync(globalThis._childProcessKill, sessionId, normalizeSignal(signal));
				child.killed = true;
				return true;
			};
			const poll = () => {
				const event = callSync(globalThis._childProcessPoll, sessionId, 0);
				if (!event) {
					setTimeout(poll, 0);
					return;
				}
				if (event.type === "stdout") {
					child.stdout.emit("data", bufferLike(event.data));
					setTimeout(poll, 0);
					return;
				}
				if (event.type === "stderr") {
					child.stderr.emit("data", bufferLike(event.data));
					setTimeout(poll, 0);
					return;
				}
				if (event.type === "exit") {
					child.exitCode = event.exitCode;
					child.signalCode = event.signal;
					child.emit("exit", event.exitCode, event.signal);
					child.emit("close", event.exitCode, event.signal);
				}
			};
			queueMicrotask(() => {
				child.emit("spawn");
				poll();
			});
			return child;
		}
		function spawnSync(command, argsOrOptions, maybeOptions) {
			const { args, options } = normalizeArgs(argsOrOptions, maybeOptions);
			try {
				const raw = callSync(
					globalThis._childProcessSpawnSync,
					{
						command: String(command),
						args: args.map(String),
						options: {
							cwd: options.cwd || (globalThis.process && globalThis.process.cwd ? globalThis.process.cwd() : "/"),
							env: options.env,
							input: encodeBytes(options.input),
						},
					},
				);
				const result = typeof raw === "string" ? JSON.parse(raw) : raw;
				const stdout = options.encoding === "utf8" || options.encoding === "utf-8" ? result.stdout : new TextEncoder().encode(result.stdout || "");
				const stderr = options.encoding === "utf8" || options.encoding === "utf-8" ? result.stderr : new TextEncoder().encode(result.stderr || "");
				return {
					pid: -1,
					output: [null, stdout, stderr],
					stdout,
					stderr,
					status: result.code,
					signal: null,
					error: undefined,
				};
			} catch (error) {
				const message = error instanceof Error ? error.message : String(error);
				const stderr = options.encoding === "utf8" || options.encoding === "utf-8" ? message : new TextEncoder().encode(message);
				return {
					pid: -1,
					output: [null, "", stderr],
					stdout: options.encoding === "utf8" || options.encoding === "utf-8" ? "" : new Uint8Array(0),
					stderr,
					status: 1,
					signal: null,
					error,
				};
			}
		}
		module.exports = { spawn, spawnSync, default: { spawn, spawnSync } };
	`,
      "node:child_process": "module.exports = require('child_process');",
      dns: `
		const callAsync = (ref, ...args) => {
			if (typeof ref === "function") return Promise.resolve(ref(...args));
			if (ref && typeof ref.apply === "function") return ref.apply(undefined, args);
			throw new Error("dns bridge is not configured");
		};
		const normalizeLookup = (hostname, options, callback) => {
			let done = callback;
			let normalized = {};
			if (typeof options === "function") {
				done = options;
			} else if (typeof options === "number") {
				normalized.family = options;
			} else if (options && typeof options === "object") {
				normalized = { ...options };
			}
			const family = normalized.family === 4 || normalized.family === 6 ? normalized.family : undefined;
			return {
				callback: done,
				options: {
					hostname: String(hostname),
					family,
					all: normalized.all === true,
				},
			};
		};
		const parseLookupRecords = (resultJson) => {
			let parsed = resultJson;
			if (typeof parsed === "string") parsed = JSON.parse(parsed);
			if (parsed && typeof parsed === "object" && Array.isArray(parsed.records)) parsed = parsed.records;
			else if (parsed && typeof parsed === "object" && typeof parsed.address === "string") parsed = [parsed];
			if (!Array.isArray(parsed)) return [];
			return parsed
				.filter((record) => record && typeof record.address === "string")
				.map((record) => ({ address: record.address, family: record.family === 6 ? 6 : 4 }));
		};
		const lookupRecords = (hostname, options, callback) => {
			const invocation = normalizeLookup(hostname, options, callback);
			return callAsync(globalThis._networkDnsLookupRaw, invocation.options)
				.then(parseLookupRecords)
				.then((records) => {
					if (typeof invocation.callback === "function") {
						if (invocation.options.all) invocation.callback(null, records);
						else {
							const first = records[0] || { address: null, family: invocation.options.family || 0 };
							invocation.callback(null, first.address, first.family);
						}
					}
					return invocation.options.all ? records : records[0] || { address: "", family: invocation.options.family || 0 };
				})
				.catch((error) => {
					if (typeof invocation.callback === "function") {
						invocation.callback(error);
						return undefined;
					}
					throw error;
				});
		};
		const promises = { lookup: (hostname, options) => lookupRecords(hostname, options) };
		function lookup(hostname, options, callback) {
			lookupRecords(hostname, options, callback);
		}
		module.exports = { lookup, promises, default: { lookup, promises } };
	`,
      "dns/promises": "module.exports = require('dns').promises;",
      dgram: `
		const encoder = new TextEncoder();
		const decoder = new TextDecoder();
		const callSync = (ref, ...args) => {
			if (typeof ref === "function") return ref(...args);
			if (ref && typeof ref.applySync === "function") return ref.applySync(undefined, args);
			if (ref && typeof ref.applySyncPromise === "function") return ref.applySyncPromise(undefined, args);
			throw new Error("dgram bridge is not configured");
		};
		const parseResult = (value) => {
			if (typeof value !== "string") return value;
			try { return JSON.parse(value); } catch { return value; }
		};
		const listenersFor = (map, event) => map.get(event) || [];
		const normalizeType = (optionsOrType) => {
			const type = typeof optionsOrType === "string" ? optionsOrType : optionsOrType && optionsOrType.type;
			if (type === "udp6") return "udp6";
			if (type === "udp4" || type === undefined) return "udp4";
			const error = new TypeError("Bad socket type specified. Valid types are: udp4, udp6");
			error.code = "ERR_SOCKET_BAD_TYPE";
			throw error;
		};
		const normalizePort = (port) => {
			const value = Number(port);
			if (!Number.isInteger(value) || value < 0 || value > 65535) {
				const error = new RangeError("Port should be >= 0 and < 65536");
				error.code = "ERR_SOCKET_BAD_PORT";
				throw error;
			}
			return value;
		};
		const normalizeMessage = (value) => {
			if (typeof value === "string") return encoder.encode(value);
			if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
			if (value instanceof ArrayBuffer) return new Uint8Array(value);
			if (Array.isArray(value)) {
				const parts = value.map(normalizeMessage);
				const total = parts.reduce((sum, part) => sum + part.byteLength, 0);
				const output = new Uint8Array(total);
				let offset = 0;
				for (const part of parts) {
					output.set(part, offset);
					offset += part.byteLength;
				}
				return output;
			}
			return encoder.encode(String(value ?? ""));
		};
		const messageBytes = (value) => {
			let bytes;
			if (value && typeof value === "object" && value.__agentOSType === "bytes" && typeof value.base64 === "string") {
				bytes = globalThis.__agentOSEncoding.base64ToBytes(value.base64);
			} else {
				bytes = normalizeMessage(value);
			}
			Object.defineProperty(bytes, "toString", {
				value() { return decoder.decode(bytes); },
				configurable: true,
			});
			return bytes;
		};
		class Socket {
			constructor(optionsOrType, callback) {
				this._type = normalizeType(optionsOrType);
				this._listeners = new Map();
				this._onceListeners = new Map();
				this._closed = false;
				this._bound = false;
				this._polling = false;
				const created = parseResult(callSync(globalThis._dgramSocketCreateRaw, { type: this._type }));
				this._socketId = String(created && created.socketId !== undefined ? created.socketId : created);
				if (typeof callback === "function") this.on("message", callback);
			}
			on(event, listener) {
				const list = listenersFor(this._listeners, event).slice();
				list.push(listener);
				this._listeners.set(event, list);
				return this;
			}
			addListener(event, listener) { return this.on(event, listener); }
			once(event, listener) {
				const list = listenersFor(this._onceListeners, event).slice();
				list.push(listener);
				this._onceListeners.set(event, list);
				return this;
			}
			off(event, listener) { return this.removeListener(event, listener); }
			removeListener(event, listener) {
				this._listeners.set(event, listenersFor(this._listeners, event).filter((entry) => entry !== listener));
				this._onceListeners.set(event, listenersFor(this._onceListeners, event).filter((entry) => entry !== listener));
				return this;
			}
			_emit(event, ...args) {
				for (const listener of listenersFor(this._listeners, event).slice()) listener(...args);
				const once = listenersFor(this._onceListeners, event).slice();
				this._onceListeners.delete(event);
				for (const listener of once) listener(...args);
				return once.length > 0 || listenersFor(this._listeners, event).length > 0;
			}
			emit(event, ...args) { return this._emit(event, ...args); }
			bind(...args) {
				let port = 0;
				let address = this._type === "udp6" ? "::" : "0.0.0.0";
				let callback;
				if (typeof args[0] === "object" && args[0] !== null) {
					port = normalizePort(args[0].port ?? 0);
					address = String(args[0].address ?? address);
					callback = args[1];
				} else {
					if (typeof args[0] === "function") callback = args[0];
					else {
						port = normalizePort(args[0] ?? 0);
						if (typeof args[1] === "string") address = args[1];
						callback = typeof args[1] === "function" ? args[1] : args[2];
					}
				}
				try {
					parseResult(callSync(globalThis._dgramSocketBindRaw, this._socketId, { port, address }));
					this._bound = true;
					queueMicrotask(() => {
						this._emit("listening");
						if (typeof callback === "function") callback.call(this);
						this._poll();
					});
				} catch (error) {
					queueMicrotask(() => this._emit("error", error));
				}
				return this;
			}
			address() {
				return parseResult(callSync(globalThis._dgramSocketAddressRaw, this._socketId));
			}
			send(message, ...args) {
				let offset = 0;
				let length;
				let port;
				let address;
				let callback;
				if (typeof args[0] === "number" && typeof args[1] === "number" && typeof args[2] === "number") {
					offset = args[0];
					length = args[1];
					port = args[2];
					address = typeof args[3] === "string" ? args[3] : undefined;
					callback = typeof args[3] === "function" ? args[3] : args[4];
				} else {
					port = args[0];
					address = typeof args[1] === "string" ? args[1] : undefined;
					callback = typeof args[1] === "function" ? args[1] : args[2];
				}
				const full = normalizeMessage(message);
				const data = length === undefined ? full : full.subarray(offset, offset + length);
				try {
					const result = parseResult(callSync(globalThis._dgramSocketSendRaw, this._socketId, data, {
						port: normalizePort(port),
						address: address || (this._type === "udp6" ? "::1" : "127.0.0.1"),
					}));
					if (typeof callback === "function") queueMicrotask(() => callback(null, result && typeof result.bytes === "number" ? result.bytes : data.length));
				} catch (error) {
					if (typeof callback === "function") queueMicrotask(() => callback(error));
					else queueMicrotask(() => this._emit("error", error));
				}
			}
			_poll() {
				if (this._closed || !this._bound || this._polling) return;
				this._polling = true;
				try {
					const event = parseResult(callSync(globalThis._dgramSocketRecvRaw, this._socketId, 10));
					if (event && event.type === "message") {
						const message = messageBytes({ __agentOSType: "bytes", base64: String(event.data || "") });
						this._emit("message", message, {
							address: event.remoteAddress,
							port: event.remotePort,
							family: event.remoteFamily || (String(event.remoteAddress).includes(":") ? "IPv6" : "IPv4"),
							size: message.length,
						});
					}
				} catch (error) {
					this._emit("error", error);
				} finally {
					this._polling = false;
				}
				if (!this._closed && this._bound) setTimeout(() => this._poll(), 10);
			}
			close(callback) {
				if (typeof callback === "function") this.once("close", callback);
				if (this._closed) return this;
				this._closed = true;
				callSync(globalThis._dgramSocketCloseRaw, this._socketId);
				queueMicrotask(() => this._emit("close"));
				return this;
			}
			ref() { return this; }
			unref() { return this; }
			setRecvBufferSize(size) { callSync(globalThis._dgramSocketSetBufferSizeRaw, this._socketId, "recv", Number(size)); }
			setSendBufferSize(size) { callSync(globalThis._dgramSocketSetBufferSizeRaw, this._socketId, "send", Number(size)); }
			getRecvBufferSize() { return Number(callSync(globalThis._dgramSocketGetBufferSizeRaw, this._socketId, "recv")); }
			getSendBufferSize() { return Number(callSync(globalThis._dgramSocketGetBufferSizeRaw, this._socketId, "send")); }
		}
		function createSocket(optionsOrType, callback) {
			return new Socket(optionsOrType, callback);
		}
		module.exports = { Socket, createSocket, default: { Socket, createSocket } };
	`,
      "node:dgram": "module.exports = require('dgram');",
      crypto: `
		const callSync = (ref, ...args) => {
			if (typeof ref === "function") return ref(...args);
			if (ref && typeof ref.applySync === "function") return ref.applySync(undefined, args);
			if (ref && typeof ref.applySyncPromise === "function") return ref.applySyncPromise(undefined, args);
			throw new Error("crypto bridge is not configured");
		};
		const encoder = new TextEncoder();
		const decoder = new TextDecoder();
		const toBytes = globalThis.__agentOSEncoding.toBytes;
		const concat = (chunks) => {
			const total = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
			const out = new Uint8Array(total);
			let offset = 0;
			for (const chunk of chunks) {
				out.set(chunk, offset);
				offset += chunk.byteLength;
			}
			return out;
		};
		const toHex = (bytes) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
		const SUPPORTED_CIPHERS = ["aes-128-cbc", "aes-128-ctr", "aes-128-gcm", "aes-192-cbc", "aes-192-ctr", "aes-192-gcm", "aes-256-cbc", "aes-256-ctr", "aes-256-gcm", "aes128", "aes192", "aes256"];
		const SUPPORTED_CURVES = ["prime256v1", "secp256k1", "secp384r1", "secp521r1"];
		const toBase64 = globalThis.__agentOSEncoding.bytesToBase64;
		const encodeOutput = (bytes, encoding) => {
			if (!encoding) return makeBuffer(bytes);
			if (encoding === "hex") return toHex(bytes);
			if (encoding === "base64") return toBase64(bytes);
			if (encoding === "utf8" || encoding === "utf-8") return decoder.decode(bytes);
			throw new Error("Unsupported crypto output encoding: " + encoding);
		};
		const makeBuffer = (bytes) => {
			if (typeof Buffer === "function") return Buffer.from(bytes);
			const out = new Uint8Array(bytes);
			out.toString = (encoding = "utf8") => encodeOutput(out, encoding);
			out.equals = (other) => {
				const rhs = toBytes(other);
				if (rhs.byteLength !== out.byteLength) return false;
				for (let i = 0; i < out.byteLength; i += 1) {
					if (out[i] !== rhs[i]) return false;
				}
				return true;
			};
			return out;
		};
		class Hash {
			constructor(algorithm) {
				this.algorithm = String(algorithm);
				this.chunks = [];
			}
			update(data, inputEncoding) {
				this.chunks.push(toBytes(data, inputEncoding));
				return this;
			}
			digest(encoding) {
				const bytes = callSync(globalThis._cryptoHashDigest, this.algorithm, concat(this.chunks));
				return encodeOutput(bytes, encoding);
			}
		}
		class Hmac {
			constructor(algorithm, key) {
				this.algorithm = String(algorithm);
				this.key = toBytes(key);
				this.chunks = [];
			}
			update(data, inputEncoding) {
				this.chunks.push(toBytes(data, inputEncoding));
				return this;
			}
			digest(encoding) {
				const bytes = callSync(globalThis._cryptoHmacDigest, this.algorithm, this.key, concat(this.chunks));
				return encodeOutput(bytes, encoding);
			}
		}
		const CRYPTO_CONSTANTS = {
			RSA_PKCS1_PADDING: 1,
			RSA_PKCS1_OAEP_PADDING: 4,
		};
		// The browser backend signs/verifies with PKCS#1 v1.5 only. Native
		// (OpenSSL) also supports RSA-PSS; rather than silently downgrade a PSS
		// request to PKCS1 (a divergence producing a different, wrong signature),
		// fail loud so the caller sees an explicit unsupported error.
		const assertSupportedSignatureKey = (key) => {
			if (key && typeof key === "object" && !ArrayBuffer.isView(key)) {
				const requestsPss =
					(key.padding !== undefined &&
						key.padding !== CRYPTO_CONSTANTS.RSA_PKCS1_PADDING) ||
					key.saltLength !== undefined;
				if (requestsPss) {
					const error = new Error(
						"ERR_UNSUPPORTED_BROWSER_CRYPTO: RSA-PSS / non-PKCS1 signature padding is not supported on the browser backend",
					);
					error.code = "ERR_UNSUPPORTED_BROWSER_CRYPTO";
					throw error;
				}
			}
		};
		const normalizeKeyInput = (key) => {
			if (typeof key === "string") return key;
			if (key && typeof key === "object" && typeof key.export === "function") return key.export({ format: "pem" });
			if (key && typeof key === "object" && typeof key.key === "string") return key.key;
			if (key && typeof key === "object" && key.key && typeof key.key.export === "function") return key.key.export({ format: "pem" });
			throw new Error("Browser node:crypto RSA key must be a PEM string");
		};
		const normalizeAsymmetricOptions = (keyOrOptions) => {
			if (typeof keyOrOptions === "string") return { key: keyOrOptions };
			if (keyOrOptions && typeof keyOrOptions === "object" && typeof keyOrOptions.export === "function") return { key: keyOrOptions };
			if (keyOrOptions && typeof keyOrOptions === "object") return keyOrOptions;
			throw new Error("Browser node:crypto RSA key must be a PEM string");
		};
		class KeyObject {
			constructor(type, key) {
				this.type = type;
				if (type === "secret") {
					this.symmetricKeySize = toBytes(key).byteLength;
					this.key = new Uint8Array(toBytes(key));
				} else if (key && typeof key === "object" && key.asymmetricKeyType === "x25519") {
					this.asymmetricKeyType = "x25519";
					this.key = new Uint8Array(toBytes(key.key));
					this.publicKey = key.publicKey ? new Uint8Array(toBytes(key.publicKey)) : undefined;
				} else {
					this.asymmetricKeyType = "rsa";
					this.key = normalizeKeyInput(key);
				}
			}
			export(options = {}) {
				if (this.type === "secret") {
					return makeBuffer(this.key);
				}
				if (this.asymmetricKeyType === "x25519") {
					throw new Error("Browser node:crypto X25519 KeyObject export is not implemented yet");
				}
				if (!options || options.format == null || options.format === "pem") return this.key;
				throw new Error("Browser node:crypto KeyObject only supports PEM export");
			}
		}
		class Sign {
			constructor(algorithm) {
				this.algorithm = String(algorithm);
				this.chunks = [];
			}
			update(data, inputEncoding) {
				this.chunks.push(toBytes(data, inputEncoding));
				return this;
			}
			write(data, inputEncoding) {
				this.update(data, inputEncoding);
				return true;
			}
			end(data, inputEncoding) {
				if (data !== undefined) this.update(data, inputEncoding);
				return this;
			}
			sign(key, outputEncoding) {
				assertSupportedSignatureKey(key);
				const bytes = callSync(globalThis._cryptoSign, this.algorithm, concat(this.chunks), normalizeKeyInput(key));
				return encodeOutput(bytes, outputEncoding);
			}
		}
		class Verify extends Sign {
			verify(key, signature, signatureEncoding) {
				assertSupportedSignatureKey(key);
				return Boolean(callSync(
					globalThis._cryptoVerify,
					this.algorithm,
					concat(this.chunks),
					normalizeKeyInput(key),
					toBytes(signature, signatureEncoding),
				));
			}
		}
		function createPrivateKey(key) {
			return new KeyObject("private", key);
		}
		function createPublicKey(key) {
			return new KeyObject("public", key);
		}
		function createSecretKey(key) {
			return new KeyObject("secret", toBytes(key));
		}
		function signOneShot(algorithm, data, key) {
			const signer = new Sign(algorithm);
			signer.update(data);
			return signer.sign(key);
		}
		function verifyOneShot(algorithm, data, key, signature) {
			const verifier = new Verify(algorithm);
			verifier.update(data);
			return verifier.verify(key, signature);
		}
		function modInverse(value, modulus) {
			let t = 0n;
			let newT = 1n;
			let r = modulus;
			let newR = mod(value, modulus);
			while (newR !== 0n) {
				const quotient = r / newR;
				const nextT = t - quotient * newT;
				t = newT;
				newT = nextT;
				const nextR = r - quotient * newR;
				r = newR;
				newR = nextR;
			}
			if (r !== 1n) throw new Error("Browser node:crypto RSA values are not invertible");
			return t < 0n ? t + modulus : t;
		}
		function gcd(left, right) {
			let a = left < 0n ? -left : left;
			let b = right < 0n ? -right : right;
			while (b !== 0n) {
				const next = a % b;
				a = b;
				b = next;
			}
			return a;
		}
		function derLength(length) {
			if (length < 0x80) return new Uint8Array([length]);
			const bytes = [];
			let remaining = length;
			while (remaining > 0) {
				bytes.unshift(remaining & 0xff);
				remaining >>= 8;
			}
			return new Uint8Array([0x80 | bytes.length, ...bytes]);
		}
		function der(tag, content) {
			return concat([new Uint8Array([tag]), derLength(content.byteLength), content]);
		}
		function derInteger(value) {
			let bytes = bigIntToMinimalBytes(value);
			if ((bytes[0] & 0x80) !== 0) bytes = concat([new Uint8Array([0]), bytes]);
			return der(0x02, bytes);
		}
		function derSequence(items) {
			return der(0x30, concat(items));
		}
		function derOctetString(bytes) {
			return der(0x04, bytes);
		}
		function derBitString(bytes) {
			return der(0x03, concat([new Uint8Array([0]), bytes]));
		}
		function derNull() {
			return new Uint8Array([0x05, 0x00]);
		}
		function derObjectIdentifier(parts) {
			const out = [parts[0] * 40 + parts[1]];
			for (const part of parts.slice(2)) {
				const stack = [part & 0x7f];
				let remaining = part >> 7;
				while (remaining > 0) {
					stack.unshift(0x80 | (remaining & 0x7f));
					remaining >>= 7;
				}
				out.push(...stack);
			}
			return der(0x06, new Uint8Array(out));
		}
		const RSA_ENCRYPTION_ALGORITHM = derSequence([
			derObjectIdentifier([1, 2, 840, 113549, 1, 1, 1]),
			derNull(),
		]);
		function pem(label, derBytes) {
			const body = toBase64(derBytes).replace(/.{1,64}/g, "$&\\n").trimEnd();
			return "-----BEGIN " + label + "-----\\n" + body + "\\n-----END " + label + "-----";
		}
		function normalizePublicExponent(value) {
			if (value === undefined) return 65537n;
			if (typeof value === "number") return BigInt(value);
			if (typeof value === "bigint") return value;
			return bytesToBigInt(toBytes(value));
		}
		function encodeRsaPublicKeyDer(key) {
			return derSequence([derInteger(key.n), derInteger(key.e)]);
		}
		function encodeRsaPrivateKeyDer(key) {
			return derSequence([
				derInteger(0n),
				derInteger(key.n),
				derInteger(key.e),
				derInteger(key.d),
				derInteger(key.p),
				derInteger(key.q),
				derInteger(key.d % (key.p - 1n)),
				derInteger(key.d % (key.q - 1n)),
				derInteger(modInverse(key.q, key.p)),
			]);
		}
		function encodeRsaSpkiDer(key) {
			return derSequence([RSA_ENCRYPTION_ALGORITHM, derBitString(encodeRsaPublicKeyDer(key))]);
		}
		function encodeRsaPkcs8Der(key) {
			return derSequence([
				derInteger(0n),
				RSA_ENCRYPTION_ALGORITHM,
				derOctetString(encodeRsaPrivateKeyDer(key)),
			]);
		}
		function encodeGeneratedRsaKey(key, encoding, defaultType) {
			if (!encoding) {
				return defaultType === "public"
					? new KeyObject("public", pem("PUBLIC KEY", encodeRsaSpkiDer(key)))
					: new KeyObject("private", pem("PRIVATE KEY", encodeRsaPkcs8Der(key)));
			}
			const format = encoding.format || "pem";
			const type = encoding.type || (defaultType === "public" ? "spki" : "pkcs8");
			let derBytes;
			let label;
			if (defaultType === "public" && type === "spki") {
				derBytes = encodeRsaSpkiDer(key);
				label = "PUBLIC KEY";
			} else if (defaultType === "public" && (type === "pkcs1" || type === "rsa")) {
				derBytes = encodeRsaPublicKeyDer(key);
				label = "RSA PUBLIC KEY";
			} else if (defaultType === "private" && type === "pkcs8") {
				derBytes = encodeRsaPkcs8Der(key);
				label = "PRIVATE KEY";
			} else if (defaultType === "private" && (type === "pkcs1" || type === "rsa")) {
				derBytes = encodeRsaPrivateKeyDer(key);
				label = "RSA PRIVATE KEY";
			} else {
				throw new Error("Browser node:crypto unsupported RSA key encoding type");
			}
			if (format === "der") return makeBuffer(derBytes);
			if (format === "pem") return pem(label, derBytes);
			throw new Error("Browser node:crypto unsupported RSA key encoding format");
		}
		function generateRsaKeyPair(options = {}) {
			const modulusLength = Number(options.modulusLength || 2048);
			if (!Number.isInteger(modulusLength) || modulusLength < 512) {
				throw new Error("Browser node:crypto RSA modulusLength must be at least 512 bits");
			}
			const e = normalizePublicExponent(options.publicExponent);
			const pBits = Math.floor(modulusLength / 2);
			const qBits = modulusLength - pBits;
			while (true) {
				const p = generatePrimeSync(pBits, { bigint: true });
				const q = generatePrimeSync(qBits, { bigint: true });
				if (p === q) continue;
				const phi = (p - 1n) * (q - 1n);
				if (gcd(e, phi) !== 1n) continue;
				const n = p * q;
				if (n.toString(2).length !== modulusLength) continue;
				const d = modInverse(e, phi);
				const key = { n, e, d, p, q };
				return {
					publicKey: encodeGeneratedRsaKey(key, options.publicKeyEncoding, "public"),
					privateKey: encodeGeneratedRsaKey(key, options.privateKeyEncoding, "private"),
				};
			}
		}
		const X25519_PRIME = (1n << 255n) - 19n;
		const X25519_A24 = 121665n;
		const X25519_BASE_POINT = new Uint8Array([9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
		function mod(value, modulus) {
			const result = value % modulus;
			return result < 0n ? result + modulus : result;
		}
		function bytesToLittleEndianBigInt(bytes) {
			let value = 0n;
			for (let i = bytes.byteLength - 1; i >= 0; i -= 1) {
				value = (value << 8n) | BigInt(bytes[i]);
			}
			return value;
		}
		function littleEndianBigIntToBytes(value, byteLength) {
			const out = new Uint8Array(byteLength);
			let cursor = BigInt(value);
			for (let i = 0; i < byteLength; i += 1) {
				out[i] = Number(cursor & 0xffn);
				cursor >>= 8n;
			}
			return out;
		}
		function normalizeX25519PrivateKey(key) {
			if (!key || key.type !== "private" || key.asymmetricKeyType !== "x25519" || key.key.byteLength !== 32) {
				throw new Error("Browser node:crypto diffieHellman requires an X25519 private KeyObject");
			}
			return key.key;
		}
		function normalizeX25519PublicKey(key) {
			if (!key || key.type !== "public" || key.asymmetricKeyType !== "x25519" || key.key.byteLength !== 32) {
				throw new Error("Browser node:crypto diffieHellman requires an X25519 public KeyObject");
			}
			return key.key;
		}
		function x25519(privateKey, publicKey) {
			const scalarBytes = new Uint8Array(privateKey);
			scalarBytes[0] &= 248;
			scalarBytes[31] &= 127;
			scalarBytes[31] |= 64;
			const uBytes = new Uint8Array(publicKey);
			uBytes[31] &= 127;
			const scalar = bytesToLittleEndianBigInt(scalarBytes);
			const x1 = bytesToLittleEndianBigInt(uBytes);
			let x2 = 1n;
			let z2 = 0n;
			let x3 = x1;
			let z3 = 1n;
			let swap = 0n;
			const cswap = (bit) => {
				if (bit === 0n) return;
				let tmp = x2;
				x2 = x3;
				x3 = tmp;
				tmp = z2;
				z2 = z3;
				z3 = tmp;
			};
			for (let t = 254; t >= 0; t -= 1) {
				const bit = (scalar >> BigInt(t)) & 1n;
				swap ^= bit;
				cswap(swap);
				swap = bit;
				const a = mod(x2 + z2, X25519_PRIME);
				const aa = mod(a * a, X25519_PRIME);
				const b = mod(x2 - z2, X25519_PRIME);
				const bb = mod(b * b, X25519_PRIME);
				const e = mod(aa - bb, X25519_PRIME);
				const c = mod(x3 + z3, X25519_PRIME);
				const d = mod(x3 - z3, X25519_PRIME);
				const da = mod(d * a, X25519_PRIME);
				const cb = mod(c * b, X25519_PRIME);
				x3 = mod((da + cb) * (da + cb), X25519_PRIME);
				z3 = mod(x1 * mod((da - cb) * (da - cb), X25519_PRIME), X25519_PRIME);
				x2 = mod(aa * bb, X25519_PRIME);
				z2 = mod(e * mod(aa + X25519_A24 * e, X25519_PRIME), X25519_PRIME);
			}
			cswap(swap);
			const result = mod(x2 * modPow(z2, X25519_PRIME - 2n, X25519_PRIME), X25519_PRIME);
			return littleEndianBigIntToBytes(result, 32);
		}
		function generateKeyPairSync(type, options = {}) {
			const keyType = String(type).toLowerCase();
			if (keyType === "rsa") {
				return generateRsaKeyPair(options || {});
			}
			if (keyType !== "x25519") {
				return unsupportedBrowserCrypto("generateKeyPairSync");
			}
			const privateBytes = new Uint8Array(callSync(globalThis._cryptoRandomFill, 32));
			const publicBytes = x25519(privateBytes, X25519_BASE_POINT);
			return {
				publicKey: new KeyObject("public", { asymmetricKeyType: "x25519", key: publicBytes }),
				privateKey: new KeyObject("private", { asymmetricKeyType: "x25519", key: privateBytes, publicKey: publicBytes }),
			};
		}
		function generateKeyPair(type, options, callback) {
			if (typeof options === "function") {
				callback = options;
				options = {};
			}
			if (typeof callback !== "function") {
				throw new TypeError('The "callback" argument must be of type function');
			}
			queueMicrotask(() => {
				try {
					const pair = generateKeyPairSync(type, options || {});
					callback(null, pair.publicKey, pair.privateKey);
				} catch (error) {
					callback(error);
				}
			});
		}
		function diffieHellman(options) {
			if (!options || typeof options !== "object") {
				throw new TypeError("Browser node:crypto diffieHellman options must be an object");
			}
			const privateKey = normalizeX25519PrivateKey(options.privateKey);
			const publicKey = normalizeX25519PublicKey(options.publicKey);
			return makeBuffer(x25519(privateKey, publicKey));
		}
		const P256_P = BigInt("0xffffffff00000001000000000000000000000000ffffffffffffffffffffffff");
		const P256_A = P256_P - 3n;
		const P256_B = BigInt("0x5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b");
		const P256_N = BigInt("0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");
		const P256_G = {
			x: BigInt("0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"),
			y: BigInt("0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"),
		};
		function p256Inverse(value) {
			return modPow(mod(value, P256_P), P256_P - 2n, P256_P);
		}
		function p256PointAdd(left, right) {
			if (!left) return right;
			if (!right) return left;
			if (left.x === right.x) {
				if (mod(left.y + right.y, P256_P) === 0n) return null;
				const slope = mod((3n * left.x * left.x + P256_A) * p256Inverse(2n * left.y), P256_P);
				const x = mod(slope * slope - 2n * left.x, P256_P);
				const y = mod(slope * (left.x - x) - left.y, P256_P);
				return { x, y };
			}
			const slope = mod((right.y - left.y) * p256Inverse(right.x - left.x), P256_P);
			const x = mod(slope * slope - left.x - right.x, P256_P);
			const y = mod(slope * (left.x - x) - left.y, P256_P);
			return { x, y };
		}
		function p256ScalarMult(scalar, point) {
			let result = null;
			let addend = point;
			let remaining = scalar;
			while (remaining > 0n) {
				if ((remaining & 1n) === 1n) result = p256PointAdd(result, addend);
				addend = p256PointAdd(addend, addend);
				remaining >>= 1n;
			}
			return result;
		}
		function p256RandomScalar() {
			while (true) {
				const scalar = bytesToBigInt(callSync(globalThis._cryptoRandomFill, 32)) % P256_N;
				if (scalar > 0n) return scalar;
			}
		}
		function p256EncodePoint(point, format = "uncompressed") {
			if (!point) throw new Error("Browser node:crypto ECDH point is not available");
			if (format === "compressed") {
				const out = new Uint8Array(33);
				out[0] = point.y & 1n ? 0x03 : 0x02;
				out.set(bigIntToBytes(point.x, 32), 1);
				return out;
			}
			if (format !== "uncompressed" && format !== "hybrid") {
				throw new Error("Browser node:crypto ECDH only supports uncompressed, compressed, and hybrid public keys");
			}
			const out = new Uint8Array(65);
			out[0] = format === "hybrid" ? (point.y & 1n ? 0x07 : 0x06) : 0x04;
			out.set(bigIntToBytes(point.x, 32), 1);
			out.set(bigIntToBytes(point.y, 32), 33);
			return out;
		}
		function p256DecodePoint(value, encoding) {
			const bytes = toBytes(value, encoding);
			if (bytes.byteLength !== 65 || (bytes[0] !== 0x04 && bytes[0] !== 0x06 && bytes[0] !== 0x07)) {
				throw new Error("Browser node:crypto ECDH peer public key must be an uncompressed P-256 point");
			}
			const x = bytesToBigInt(bytes.subarray(1, 33));
			const y = bytesToBigInt(bytes.subarray(33, 65));
			if (mod(y * y - (x * x * x + P256_A * x + P256_B), P256_P) !== 0n) {
				throw new Error("Browser node:crypto ECDH peer public key is not on P-256");
			}
			return { x, y };
		}
		class ECDH {
			constructor(name) {
				const curve = String(name);
				if (curve !== "prime256v1" && curve !== "P-256") {
					const error = new Error("Invalid EC curve name");
					error.code = "ERR_CRYPTO_INVALID_CURVE";
					throw error;
				}
				this.privateKey = null;
				this.publicPoint = null;
			}
			generateKeys(encoding, format = "uncompressed") {
				this.privateKey = p256RandomScalar();
				this.publicPoint = p256ScalarMult(this.privateKey, P256_G);
				return encodeOutput(p256EncodePoint(this.publicPoint, format), encoding);
			}
			computeSecret(otherPublicKey, inputEncoding, outputEncoding) {
				if (this.privateKey === null) this.generateKeys();
				const shared = p256ScalarMult(this.privateKey, p256DecodePoint(otherPublicKey, inputEncoding));
				if (!shared) throw new Error("Browser node:crypto ECDH failed to compute shared secret");
				return encodeOutput(bigIntToBytes(shared.x, 32), outputEncoding);
			}
			getPublicKey(encoding, format = "uncompressed") {
				if (!this.publicPoint) throw new Error("Failed to get ECDH public key");
				return encodeOutput(p256EncodePoint(this.publicPoint, format), encoding);
			}
			getPrivateKey(encoding) {
				if (this.privateKey === null) throw new Error("Failed to get ECDH private key");
				return encodeOutput(bigIntToBytes(this.privateKey, 32), encoding);
			}
			setPrivateKey(privateKey, encoding) {
				const scalar = bytesToBigInt(toBytes(privateKey, encoding));
				if (scalar <= 0n || scalar >= P256_N) throw new Error("Invalid ECDH private key");
				this.privateKey = scalar;
				this.publicPoint = p256ScalarMult(this.privateKey, P256_G);
			}
			setPublicKey(publicKey, encoding) {
				this.publicPoint = p256DecodePoint(publicKey, encoding);
			}
		}
		function createECDH(name) {
			return new ECDH(name);
		}
		function generateKeySync(type, options = {}) {
			const keyType = String(type).toLowerCase();
			const length = Number(options && options.length);
			if (!Number.isInteger(length) || length <= 0) {
				throw new Error("Browser node:crypto generateKeySync length must be a positive integer");
			}
			if (keyType === "aes" && ![128, 192, 256].includes(length)) {
				const error = new Error("The property 'options.length' must be one of: 128, 192, 256.");
				error.code = "ERR_INVALID_ARG_VALUE";
				throw error;
			}
			if (keyType !== "hmac" && keyType !== "aes") {
				return unsupportedBrowserCrypto("generateKeySync");
			}
			return createSecretKey(callSync(globalThis._cryptoRandomFill, Math.ceil(length / 8)));
		}
		function bytesToBigInt(bytes) {
			let value = 0n;
			for (const byte of bytes) value = (value << 8n) | BigInt(byte);
			return value;
		}
		function bigIntToBytes(value, byteLength) {
			const out = new Uint8Array(byteLength);
			let cursor = BigInt(value);
			for (let i = byteLength - 1; i >= 0; i -= 1) {
				out[i] = Number(cursor & 0xffn);
				cursor >>= 8n;
			}
			return out;
		}
		function normalizePrimeOption(name, value) {
			if (value === undefined) return undefined;
			if (typeof value === "bigint") return value;
			if (ArrayBuffer.isView(value) || value instanceof ArrayBuffer || Array.isArray(value) || (value && value.type === "Buffer" && Array.isArray(value.data))) {
				return bytesToBigInt(toBytes(value));
			}
			const error = new TypeError('The "options.' + name + '" property must be of type bigint or an instance of ArrayBuffer, TypedArray, Buffer, or DataView.');
			error.code = "ERR_INVALID_ARG_TYPE";
			throw error;
		}
		function modPow(base, exponent, modulus) {
			let result = 1n;
			let cursor = base % modulus;
			let remaining = exponent;
			while (remaining > 0n) {
				if ((remaining & 1n) === 1n) result = (result * cursor) % modulus;
				cursor = (cursor * cursor) % modulus;
				remaining >>= 1n;
			}
			return result;
		}
		const SMALL_PRIMES = [2n, 3n, 5n, 7n, 11n, 13n, 17n, 19n, 23n, 29n, 31n, 37n, 41n, 43n, 47n, 53n, 59n, 61n, 67n, 71n, 73n, 79n, 83n, 89n, 97n];
		const MILLER_RABIN_BASES = [2n, 3n, 5n, 7n, 11n, 13n, 17n, 19n, 23n, 29n, 31n, 37n];
		function isProbablePrime(value) {
			if (value < 2n) return false;
			for (const prime of SMALL_PRIMES) {
				if (value === prime) return true;
				if (value % prime === 0n) return false;
			}
			let d = value - 1n;
			let s = 0;
			while ((d & 1n) === 0n) {
				d >>= 1n;
				s += 1;
			}
			for (const base of MILLER_RABIN_BASES) {
				if (base >= value - 2n) continue;
				let x = modPow(base, d, value);
				if (x === 1n || x === value - 1n) continue;
				let witness = false;
				for (let r = 1; r < s; r += 1) {
					x = (x * x) % value;
					if (x === value - 1n) {
						witness = true;
						break;
					}
				}
				if (!witness) return false;
			}
			return true;
		}
		function randomPrimeCandidate(size, add, rem) {
			const byteLength = Math.ceil(size / 8);
			const mask = (1n << BigInt(size)) - 1n;
			const highBit = 1n << BigInt(size - 1);
			let candidate = (bytesToBigInt(callSync(globalThis._cryptoRandomFill, byteLength)) & mask) | highBit;
			if (add !== undefined) {
				const desired = rem === undefined ? 1n : rem;
				const delta = (desired - (candidate % add) + add) % add;
				candidate += delta;
				if (candidate > mask) candidate -= add;
			} else {
				candidate |= 1n;
			}
			return candidate;
		}
		function generatePrimeSync(size, options = {}) {
			const bitLength = Number(size);
			if (!Number.isInteger(bitLength) || bitLength < 2) {
				throw new RangeError("Browser node:crypto generatePrimeSync size must be an integer greater than 1");
			}
			if (bitLength > 4096) {
				throw new RangeError("Browser node:crypto generatePrimeSync supports primes up to 4096 bits");
			}
			const primeOptions = options || {};
			const add = normalizePrimeOption("add", primeOptions.add);
			const rem = normalizePrimeOption("rem", primeOptions.rem);
			if (add !== undefined && add <= 0n) {
				throw new RangeError("Browser node:crypto generatePrimeSync options.add must be greater than zero");
			}
			if (rem !== undefined && add === undefined) {
				throw new RangeError("Browser node:crypto generatePrimeSync options.rem requires options.add");
			}
			const safe = primeOptions.safe === true;
			while (true) {
				const candidate = randomPrimeCandidate(bitLength, add, rem);
				if (candidate < 2n || candidate.toString(2).length !== bitLength) continue;
				if (!isProbablePrime(candidate)) continue;
				if (safe && !isProbablePrime((candidate - 1n) / 2n)) continue;
				if (primeOptions.bigint === true) return candidate;
				const bytes = bigIntToBytes(candidate, Math.ceil(bitLength / 8));
				return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
			}
		}
		const DIFFIE_HELLMAN_GROUPS = {
			modp14: {
				prime: "ffffffffffffffffc90fdaa22168c234c4c6628b80dc1cd129024e088a67cc74020bbea63b139b22514a08798e3404ddef9519b3cd3a431b302b0a6df25f14374fe1356d6d51c245e485b576625e7ec6f44c42e9a637ed6b0bff5cb6f406b7edee386bfb5a899fa5ae9f24117c4b1fe649286651ece45b3dc2007cb8a163bf0598da48361c55d39a69163fa8fd24cf5f83655d23dca3ad961c62f356208552bb9ed529077096966d670c354e4abc9804f1746c08ca18217c32905e462e36ce3be39e772c180e86039b2783a2ec07a28fb5c55df06f4c52c9de2bcbf6955817183995497cea956ae515d2261898fa051015728e5a8aacaa68ffffffffffffffff",
				generator: 2n,
			},
		};
		function bigIntToMinimalBytes(value) {
			if (value === 0n) return new Uint8Array([0]);
			return bigIntToBytes(value, Math.ceil(value.toString(16).length / 2));
		}
		function normalizeDhNumber(value, encoding) {
			if (typeof value === "bigint") return value;
			if (typeof value === "number") return BigInt(value);
			return bytesToBigInt(toBytes(value, encoding));
		}
		class DiffieHellman {
			constructor(prime, generator = 2n) {
				this.prime = BigInt(prime);
				this.generator = BigInt(generator);
				this.primeLength = Math.ceil(this.prime.toString(2).length / 8);
				this.privateKey = null;
				this.publicKey = null;
				this.verifyError = 0;
			}
			_generatePrivateKey() {
				const randomLength = Math.min(this.primeLength, 32);
				const random = bytesToBigInt(callSync(globalThis._cryptoRandomFill, randomLength));
				return 2n + (random % (this.prime - 3n));
			}
			generateKeys(encoding) {
				this.privateKey = this._generatePrivateKey();
				this.publicKey = modPow(this.generator, this.privateKey, this.prime);
				return encodeOutput(bigIntToBytes(this.publicKey, this.primeLength), encoding);
			}
			computeSecret(otherPublicKey, inputEncoding, outputEncoding) {
				if (this.privateKey === null) this.generateKeys();
				const peer = normalizeDhNumber(otherPublicKey, inputEncoding);
				const secret = modPow(peer, this.privateKey, this.prime);
				return encodeOutput(bigIntToBytes(secret, this.primeLength), outputEncoding);
			}
			getPrime(encoding) {
				return encodeOutput(bigIntToBytes(this.prime, this.primeLength), encoding);
			}
			getGenerator(encoding) {
				return encodeOutput(bigIntToMinimalBytes(this.generator), encoding);
			}
			getPublicKey(encoding) {
				if (this.publicKey === null) this.generateKeys();
				return encodeOutput(bigIntToBytes(this.publicKey, this.primeLength), encoding);
			}
			getPrivateKey(encoding) {
				if (this.privateKey === null) this.generateKeys();
				return encodeOutput(bigIntToMinimalBytes(this.privateKey), encoding);
			}
			setPublicKey(key, encoding) {
				this.publicKey = normalizeDhNumber(key, encoding);
			}
			setPrivateKey(key, encoding) {
				this.privateKey = normalizeDhNumber(key, encoding);
				this.publicKey = modPow(this.generator, this.privateKey, this.prime);
			}
		}
		function createDiffieHellman(prime, primeEncoding, generator, generatorEncoding) {
			let normalizedGenerator = generator;
			let normalizedGeneratorEncoding = generatorEncoding;
			if (typeof primeEncoding !== "string") {
				normalizedGenerator = primeEncoding === undefined ? generator : primeEncoding;
				normalizedGeneratorEncoding = typeof generator === "string" ? generator : undefined;
				primeEncoding = undefined;
			}
			const primeValue = normalizeDhNumber(prime, primeEncoding);
			const generatorValue = normalizedGenerator === undefined
				? 2n
				: normalizeDhNumber(normalizedGenerator, normalizedGeneratorEncoding);
			return new DiffieHellman(primeValue, generatorValue);
		}
		function getDiffieHellman(name) {
			const group = DIFFIE_HELLMAN_GROUPS[String(name).toLowerCase()];
			if (!group) {
				const error = new Error("Unknown DH group");
				error.code = "ERR_CRYPTO_UNKNOWN_DH_GROUP";
				throw error;
			}
			return new DiffieHellman(bytesToBigInt(toBytes(group.prime, "hex")), group.generator);
		}
		function publicEncrypt(keyOrOptions, buffer) {
			const options = normalizeAsymmetricOptions(keyOrOptions);
			const bytes = callSync(
				globalThis._cryptoAsymmetricOp,
				"publicEncrypt",
				normalizeKeyInput(options.key),
				toBytes(buffer),
				JSON.stringify({
					padding: options.padding,
					oaepHash: options.oaepHash,
					oaepLabel: options.oaepLabel ? Array.from(toBytes(options.oaepLabel)) : undefined,
				}),
			);
			return makeBuffer(bytes);
		}
		function privateDecrypt(keyOrOptions, buffer) {
			const options = normalizeAsymmetricOptions(keyOrOptions);
			const bytes = callSync(
				globalThis._cryptoAsymmetricOp,
				"privateDecrypt",
				normalizeKeyInput(options.key),
				toBytes(buffer),
				JSON.stringify({
					padding: options.padding,
					oaepHash: options.oaepHash,
					oaepLabel: options.oaepLabel ? Array.from(toBytes(options.oaepLabel)) : undefined,
				}),
			);
			return makeBuffer(bytes);
		}
		function randomBytes(size, callback) {
			const bytes = makeBuffer(callSync(globalThis._cryptoRandomFill, Number(size)));
			if (typeof callback === "function") queueMicrotask(() => callback(null, bytes));
			return bytes;
		}
		function randomFillSync(buffer, offset = 0, size) {
			const view = toBytes(buffer);
			const start = Number(offset) || 0;
			const length = size == null ? view.byteLength - start : Number(size);
			view.set(callSync(globalThis._cryptoRandomFill, length), start);
			return buffer;
		}
		function pbkdf2Sync(password, salt, iterations, keyLength, digest = "sha1") {
			return makeBuffer(callSync(
				globalThis._cryptoPbkdf2,
				toBytes(password),
				toBytes(salt),
				Number(iterations),
				Number(keyLength),
				String(digest),
			));
		}
		function pbkdf2(password, salt, iterations, keyLength, digest, callback) {
			if (typeof digest === "function") {
				callback = digest;
				digest = "sha1";
			}
			queueMicrotask(() => {
				try {
					callback(null, pbkdf2Sync(password, salt, iterations, keyLength, digest || "sha1"));
				} catch (error) {
					callback(error);
				}
			});
		}
		function scryptSync(password, salt, keyLength, options = undefined) {
			return makeBuffer(callSync(
				globalThis._cryptoScrypt,
				toBytes(password),
				toBytes(salt),
				Number(keyLength),
				options || {},
			));
		}
		function scrypt(password, salt, keyLength, options, callback) {
			if (typeof options === "function") {
				callback = options;
				options = undefined;
			}
			if (typeof callback !== "function") {
				throw new TypeError('The "callback" argument must be of type function');
			}
			queueMicrotask(() => {
				try {
					callback(null, scryptSync(password, salt, keyLength, options));
				} catch (error) {
					callback(error);
				}
			});
		}
		class Cipheriv {
			constructor(mode, algorithm, key, iv, options = {}) {
				this.mode = mode;
				this.algorithm = String(algorithm);
				this.key = toBytes(key);
				this.iv = toBytes(iv);
				this.options = { ...(options || {}) };
				this.chunks = [];
				this.finished = false;
				this.authTag = null;
			}
			update(data, inputEncoding, outputEncoding) {
				if (this.finished) throw new Error("Cipheriv final already called");
				this.chunks.push(toBytes(data, inputEncoding));
				return encodeOutput(new Uint8Array(0), outputEncoding);
			}
			final(outputEncoding) {
				if (this.finished) throw new Error("Cipheriv final already called");
				this.finished = true;
				const input = concat(this.chunks);
				let result;
				if (this.mode === "cipher") {
					result = callSync(globalThis._cryptoCipheriv, this.algorithm, this.key, this.iv, input, this.options);
					if (this.algorithm.toLowerCase().endsWith("-gcm")) {
						this.authTag = result.slice(result.byteLength - 16);
						result = result.slice(0, result.byteLength - 16);
					}
				} else {
					result = callSync(globalThis._cryptoDecipheriv, this.algorithm, this.key, this.iv, input, this.options);
				}
				return encodeOutput(result, outputEncoding);
			}
			setAutoPadding(autoPadding = true) {
				if (this.finished) throw new Error("Cipheriv final already called");
				this.options.autoPadding = autoPadding !== false;
				return this;
			}
			setAAD(aad) {
				if (this.finished) throw new Error("Cipheriv final already called");
				this.options.aad = toBytes(aad);
				return this;
			}
			getAuthTag() {
				if (!this.authTag) throw new Error("Cipheriv auth tag is not available");
				return makeBuffer(this.authTag);
			}
			setAuthTag(tag) {
				if (this.finished) throw new Error("Cipheriv final already called");
				this.options.authTag = toBytes(tag);
				return this;
			}
		}
		function unsupportedBrowserCrypto(operation) {
			const error = new Error("node:crypto " + operation + " is not implemented in the browser runtime yet");
			error.code = "ERR_UNSUPPORTED_BROWSER_CRYPTO";
			throw error;
		}
		module.exports = {
			createCipheriv: (algorithm, key, iv, options) => new Cipheriv("cipher", algorithm, key, iv, options),
			createDecipheriv: (algorithm, key, iv, options) => new Cipheriv("decipher", algorithm, key, iv, options),
			createDiffieHellman,
			createECDH,
			createHash: (algorithm) => new Hash(algorithm),
			createHmac: (algorithm, key) => new Hmac(algorithm, key),
			constants: CRYPTO_CONSTANTS,
			createPrivateKey,
			createPublicKey,
			createSecretKey,
			createSign: (algorithm) => new Sign(algorithm),
			createVerify: (algorithm) => new Verify(algorithm),
			diffieHellman,
			generateKeyPair,
			generateKeyPairSync,
			generateKeySync,
			generatePrimeSync,
			getCiphers: () => [...SUPPORTED_CIPHERS],
			getCurves: () => [...SUPPORTED_CURVES],
			getDiffieHellman,
			getHashes: () => ["md5", "sha1", "sha224", "sha256", "sha384", "sha512"],
			pbkdf2,
			pbkdf2Sync,
			privateDecrypt,
			publicEncrypt,
			randomBytes,
			randomFillSync,
			randomUUID: () => callSync(globalThis._cryptoRandomUUID),
			scrypt,
			scryptSync,
			sign: signOneShot,
			subtle: globalThis.crypto && globalThis.crypto.subtle,
			verify: verifyOneShot,
			webcrypto: globalThis.crypto,
		};
	`,
      "node:crypto": "module.exports = require('crypto');",
      wasi: BROWSER_WASI_POLYFILL_CODE,
      "node:wasi": "module.exports = require('wasi');",
      "secure-exec:wasi-command-host": `
		function defaultDecode(bytes) {
			return new TextDecoder().decode(bytes);
		}
		function decodeNullSeparated(bytes) {
			const out = [];
			let start = 0;
			for (let i = 0; i <= bytes.length; i += 1) {
				if (i === bytes.length || bytes[i] === 0) {
					if (i > start) out.push(defaultDecode(bytes.slice(start, i)));
					start = i + 1;
				}
			}
			return out;
		}
		function parseEnv(bytes) {
			const env = {};
			for (const entry of decodeNullSeparated(bytes)) {
				const eq = entry.indexOf("=");
				if (eq > 0) env[entry.slice(0, eq)] = entry.slice(eq + 1);
			}
			return env;
		}
		async function readCommandBytes(source) {
			if (source instanceof Uint8Array) return source;
			if (source instanceof ArrayBuffer) return new Uint8Array(source);
			if (source instanceof WebAssembly.Module) return source;
			if (typeof source !== "string") throw new Error("command source must be a URL, bytes, or WebAssembly.Module");
			const response = await fetch(source);
			if (!response.ok) throw new Error("failed to fetch command wasm " + source + ": " + response.status);
			let bytes = new Uint8Array(await response.arrayBuffer());
			if (response.headers && response.headers.get("x-body-encoding") === "base64") {
				const encoded = new TextDecoder().decode(bytes);
				bytes = Uint8Array.from(atob(encoded), (char) => char.charCodeAt(0));
			}
			return bytes;
		}
		async function loadCommandModules(commands) {
			const modules = new Map();
			for (const [name, source] of Object.entries(commands || {})) {
				const value = await readCommandBytes(source);
				modules.set(name, value instanceof WebAssembly.Module ? value : new WebAssembly.Module(value));
			}
			return modules;
		}
		async function createWasiCommandHost(options) {
			const WASI = options && options.WASI ? options.WASI : require("node:wasi").WASI;
			const commandModules = await loadCommandModules(options && options.commands);
			let memory = null;
			let nextPid = 100;
			const exitedChildren = new Map();
			const deferredChildren = new Map();
			const waitBuffer = new SharedArrayBuffer(4);
			const wait = new Int32Array(waitBuffer);
			const errnoSuccess = 0;
			const errnoBadf = 8;
			const errnoChild = 10;
			const errnoNosys = 52;
			let nextSyntheticFd = 1000;
			const syntheticFdEntries = new Map();
			let activeFdOverrides = null;
			let activeChildCwd = null;
			let previousLookupFdHandle = null;
			let parentWasi = null;
			const getMemory = () => {
				if (!memory) throw new Error("WASI host command memory is not set");
				return memory;
			};
			const view = () => new DataView(getMemory().buffer);
			const bytes = () => new Uint8Array(getMemory().buffer);
			const writeU32 = (ptr, value) => {
				view().setUint32(ptr >>> 0, value >>> 0, true);
				return errnoSuccess;
			};
			const writeBytes = (ptr, value) => {
				bytes().set(value, ptr >>> 0);
			};
			const readBytes = (ptr, len) => bytes().slice(ptr >>> 0, (ptr >>> 0) + (len >>> 0));
			const readString = (ptr, len) => defaultDecode(readBytes(ptr, len));
			const fs = () => require("node:fs");
			const path = () => require("node:path");
			const userRecord = new TextEncoder().encode(
				(options && options.userRecord) || "agentos:x:1000:1000:Agent OS:/tmp:/bin/sh",
			);
			const modeFromStat = (stat, fallback) => {
				const mode = Number(stat && stat.mode);
				if (Number.isInteger(mode) && mode > 0) return mode >>> 0;
				if (stat && typeof stat.isDirectory === "function" && stat.isDirectory()) return 0o040755;
				if (stat && typeof stat.isSymbolicLink === "function" && stat.isSymbolicLink()) return 0o120777;
				return fallback >>> 0;
			};
			const currentGuestCwd = () => {
				const cwd = typeof activeChildCwd === "string" && activeChildCwd.startsWith("/")
					? activeChildCwd
					: typeof options?.cwd === "string" && options.cwd.startsWith("/")
					? options.cwd
					: "/";
				return path().posix.normalize(cwd);
			};
			const resolveGuestPath = (target) => {
				const value = String(target || ".");
				return value.startsWith("/")
					? path().posix.normalize(value)
					: path().posix.resolve(currentGuestCwd(), value);
			};
			const lookupSyntheticFd = (fd) => {
				const descriptor = fd >>> 0;
				const override = activeFdOverrides && activeFdOverrides.get(descriptor);
				if (override && override.open !== false) return override;
				const handle = syntheticFdEntries.get(descriptor);
				if (handle && handle.open !== false) return handle;
				if (typeof previousLookupFdHandle === "function") return previousLookupFdHandle(descriptor);
				const parentEntry = parentWasi && parentWasi.fdTable && parentWasi.fdTable.get(descriptor);
				if (parentEntry && parentEntry.kind === "file" && typeof parentEntry.realFd === "number") {
					return {
						kind: "guest-file",
						targetFd: parentEntry.realFd,
						position: typeof parentEntry.offset === "number" ? parentEntry.offset : 0,
						readOnly: parentEntry.readOnly === true,
						open: true,
					};
				}
				return null;
			};
			const closeSyntheticHandle = (handle) => {
				if (!handle || handle.open === false) return;
				handle.open = false;
				if (handle.kind === "pipe-read" && handle.pipe) {
					handle.pipe.readHandleCount = Math.max(0, (handle.pipe.readHandleCount || 0) - 1);
				} else if (handle.kind === "pipe-write" && handle.pipe) {
					handle.pipe.writeHandleCount = Math.max(0, (handle.pipe.writeHandleCount || 0) - 1);
				}
				if (typeof handle.onClose === "function") handle.onClose(handle);
			};
			const cloneSyntheticHandle = (handle) => {
				if (!handle || handle.open === false) return null;
				if (handle.kind === "stdio") {
					return { kind: "stdio", targetFd: handle.targetFd, open: true };
				}
				if (handle.kind === "guest-file") {
					return { ...handle, open: true };
				}
				if (!handle.pipe) return null;
				if (handle.kind === "pipe-read") {
					handle.pipe.readHandleCount = (handle.pipe.readHandleCount || 0) + 1;
					return { kind: "pipe-read", pipe: handle.pipe, open: true, onClose: handle.onClose };
				}
				if (handle.kind === "pipe-write") {
					handle.pipe.writeHandleCount = (handle.pipe.writeHandleCount || 0) + 1;
					return { kind: "pipe-write", pipe: handle.pipe, open: true, onClose: handle.onClose };
				}
				return null;
			};
			const handleMatchesStdio = (handle, expectedKind) => {
				if (!handle || handle.open === false) return false;
				if (handle.kind === "stdio") {
					if (expectedKind === "read") return handle.targetFd === 0;
					if (expectedKind === "write") return handle.targetFd === 1 || handle.targetFd === 2;
				}
				if (expectedKind === "read") return handle.kind === "pipe-read" || handle.kind === "guest-file";
				if (expectedKind === "write") return handle.kind === "pipe-write" || handle.kind === "guest-file";
				return handle.kind === expectedKind;
			};
			const allocateSyntheticFd = (handle) => {
				const fd = nextSyntheticFd++;
				syntheticFdEntries.set(fd, handle);
				return fd;
			};
			const replaceSyntheticFd = (fd, handle) => {
				const descriptor = fd >>> 0;
				closeSyntheticHandle(syntheticFdEntries.get(descriptor));
				syntheticFdEntries.set(descriptor, handle);
			};
			const pipeHasOpenWriters = (handle) =>
				handle && handle.kind === "pipe-read" && handle.pipe && (handle.pipe.writeHandleCount || 0) > 0;
			const runChild = (child) => {
				const parentMemory = memory;
				const previousActiveFdOverrides = activeFdOverrides;
				const previousActiveChildCwd = activeChildCwd;
				try {
					const childWasi = new WASI({
						returnOnExit: true,
						args: [child.commandPath, ...child.argv.slice(1)],
						env: child.env,
						preopens: { "/": child.cwd || "/" },
					});
					const childImports = {
						wasi_snapshot_preview1: childWasi.wasiImport,
						...host.imports,
					};
					const childInstance = new WebAssembly.Instance(child.module, childImports);
					memory = childInstance.exports.memory;
					activeFdOverrides = child.overrides;
					activeChildCwd = child.cwd || "/";
					const exitCode = childWasi.start(childInstance);
					exitedChildren.set(child.pid, exitCode << 8);
				} catch {
					exitedChildren.set(child.pid, 127 << 8);
				} finally {
					for (const handle of child.childOverrideHandles) closeSyntheticHandle(handle);
					activeFdOverrides = previousActiveFdOverrides;
					activeChildCwd = previousActiveChildCwd;
					memory = parentMemory;
				}
			};
			const runReadyDeferredChildren = (requestedPid) => {
				let ran = false;
				for (const [pid, child] of Array.from(deferredChildren.entries())) {
					if (requestedPid && pid !== requestedPid) continue;
					const stdinHandle = child.overrides.get(0);
					if (pipeHasOpenWriters(stdinHandle)) continue;
					deferredChildren.delete(pid);
					runChild(child);
					ran = true;
				}
				return ran;
			};
			const onPipeHandleClose = () => {
				while (runReadyDeferredChildren()) {
					// Keep draining children made ready by the previous child exit.
				}
			};
			const host = {
				setMemory(nextMemory) {
					memory = nextMemory;
					return host;
				},
				setParentWasi(wasi) {
					parentWasi = wasi || null;
					return host;
				},
				installBlockingStdin(processLike) {
					const target = processLike || globalThis.process;
					const wasiHost = globalThis.__agentOSWasiHost || (globalThis.__agentOSWasiHost = {});
					wasiHost.readStdin = (maxBytes) => {
						while (true) {
							const value = target && target.stdin && typeof target.stdin.read === "function"
								? target.stdin.read(maxBytes)
								: null;
							const length = typeof value === "string"
								? value.length
								: value instanceof Uint8Array
									? value.byteLength
									: value && typeof value.byteLength === "number"
										? value.byteLength
										: 0;
							if (length > 0) return value;
							Atomics.wait(wait, 0, 0, 10);
						}
					};
					wasiHost.readStdinNonBlocking = (maxBytes) =>
							target && target.stdin && typeof target.stdin.read === "function"
								? target.stdin.read(maxBytes)
								: null;
						wasiHost.stdinReadableBytes = () => 1;
					if (typeof wasiHost.lookupFdHandle === "function" && wasiHost.lookupFdHandle !== lookupSyntheticFd) {
						previousLookupFdHandle = wasiHost.lookupFdHandle;
					}
					wasiHost.lookupFdHandle = lookupSyntheticFd;
					return host;
				},
				imports: {
					host_tty: {
						// crossterm WasiEventSource keystroke source: read(ptr, len, timeout_ms) -> usize.
						// usize::MAX (-1 as i32) means block until input; the brush/reedline read loop
						// polls with None (blocking), so we wait on the kernel PTY stdin and copy bytes
						// into guest memory, returning the count. Short/zero timeouts report "no event"
						// (0); the guest then falls back to its blocking read.
						read(ptr, len, timeoutMs) {
							const cap = len >>> 0;
							if (cap === 0) return 0;
							const wasiHost = globalThis.__agentOSWasiHost;
							if (!wasiHost) return 0;
							const blocking = (timeoutMs >>> 0) === 0xffffffff;
							const budget = blocking ? Infinity : (timeoutMs >>> 0);
							const toBytes = (value) => {
								if (typeof value === "string") return new TextEncoder().encode(value);
								if (value instanceof Uint8Array) return value;
								if (value && typeof value.byteLength === "number")
									return new Uint8Array(value.buffer || value, value.byteOffset || 0, value.byteLength);
								return null;
							};
							let waited = 0;
							for (;;) {
								// Prefer a single non-blocking read so finite timeouts (e.g. crossterm's
								// cursor-position report) can return promptly with whatever is queued.
								const value = typeof wasiHost.readStdinNonBlocking === "function"
									? wasiHost.readStdinNonBlocking(cap)
									: null;
								const bytes = toBytes(value);
								if (bytes && bytes.length > 0) {
									const n = Math.min(bytes.length, cap);
									writeBytes(ptr, bytes.subarray(0, n));
									return n;
								}
								if (!blocking && waited >= budget) return 0;
								const step = blocking ? 10 : Math.max(1, Math.min(10, budget - waited));
								Atomics.wait(wait, 0, 0, step);
								waited += step;
							}
						},
						// Toggle terminal raw mode on the guest's PTY. crossterm calls this instead
						// of tcsetattr; route it to the kernel via process.stdin.setRawMode (which
						// drives __pty_set_raw_mode), so reedline gets raw \r keystrokes and submits
						// commands. Returns errno 0.
						set_raw_mode(_enabled) {
							return 0;
						},
					},
					host_user: {
						getuid(ret) { return writeU32(ret, 1000); },
						getgid(ret) { return writeU32(ret, 1000); },
						geteuid(ret) { return writeU32(ret, 1000); },
						getegid(ret) { return writeU32(ret, 1000); },
						isatty(fd, ret) {
							return writeU32(ret, fd === 0 || fd === 1 || fd === 2 ? 1 : 0);
						},
						getpwuid(_uid, bufPtr, bufLen, retLen) {
							const len = Math.min(userRecord.length, bufLen >>> 0);
							writeBytes(bufPtr, userRecord.subarray(0, len));
							writeU32(retLen, len);
							return errnoSuccess;
						},
					},
					host_fs: {
						fd_mode(fd) {
							const descriptor = fd >>> 0;
							if (descriptor <= 2) return 0o020666;
							const handle = lookupSyntheticFd(descriptor);
							if (handle && (handle.kind === "pipe-read" || handle.kind === "pipe-write")) return 0o010600;
							if (handle && handle.kind === "guest-file" && typeof handle.targetFd === "number") {
								try {
									return modeFromStat(fs().fstatSync(handle.targetFd), 0o100644);
								} catch {
									return 0o100644;
								}
							}
							const parentEntry = parentWasi && parentWasi.fdTable && parentWasi.fdTable.get(descriptor);
							if (parentEntry && (parentEntry.kind === "preopen" || parentEntry.kind === "directory")) return 0o040755;
							if (parentEntry && parentEntry.kind === "file" && typeof parentEntry.realFd === "number") {
								try {
									return modeFromStat(fs().fstatSync(parentEntry.realFd), 0o100644);
								} catch {
									return 0o100644;
								}
							}
							return 0o100644;
						},
						path_mode(pathPtr, pathLen, followSymlinks) {
							try {
								const guestPath = resolveGuestPath(readString(pathPtr, pathLen));
								const stat = Number(followSymlinks) === 0
									? fs().lstatSync(guestPath)
									: fs().statSync(guestPath);
								return modeFromStat(stat, 0o100644);
							} catch {
								return 0;
							}
						},
					},
					host_process: {
						proc_spawn(argvPtr, argvLen, envpPtr, envpLen, stdinFd, stdoutFd, stderrFd, cwdPtr, cwdLen, retPid) {
							try {
								const argv = decodeNullSeparated(readBytes(argvPtr, argvLen));
								if (argv.length === 0) return errnoNosys;
								const commandPath = argv[0];
								const commandName = commandPath.split("/").filter(Boolean).at(-1) || commandPath;
								const module = commandModules.get(commandName);
								if (!module) return errnoNosys;
								const env = {
									...(options && options.env ? options.env : {}),
									...parseEnv(readBytes(envpPtr, envpLen)),
									PATH: (options && options.path) || "/bin:/usr/bin",
								};
								const cwd = cwdLen ? readString(cwdPtr, cwdLen) : ((options && options.cwd) || "/");
								const childOverrideHandles = [];
								const overrides = new Map();
								for (const [childFd, parentFd, expectedKind] of [
									[0, stdinFd >>> 0, "read"],
									[1, stdoutFd >>> 0, "write"],
									[2, stderrFd >>> 0, "write"],
								]) {
									const parentHandle = lookupSyntheticFd(parentFd);
									if (parentFd <= 2 && !parentHandle) continue;
									if (!handleMatchesStdio(parentHandle, expectedKind)) return errnoBadf;
									const childHandle = cloneSyntheticHandle(parentHandle);
									if (!childHandle) return errnoBadf;
									overrides.set(childFd, childHandle);
									childOverrideHandles.push(childHandle);
								}
								const pid = nextPid++;
								const child = { pid, module, commandPath, argv, env, cwd, overrides, childOverrideHandles };
								if (pipeHasOpenWriters(overrides.get(0))) {
									deferredChildren.set(pid, child);
								} else {
									runChild(child);
								}
								return writeU32(retPid, pid);
							} catch {
								return errnoNosys;
							}
						},
						proc_waitpid(pid, _options, retStatus, retPid) {
							const requested = pid >>> 0;
							runReadyDeferredChildren(requested === 0xffffffff ? undefined : requested);
							const childPid = requested === 0xffffffff
								? exitedChildren.keys().next().value
								: requested;
							if (!childPid || !exitedChildren.has(childPid)) {
								writeU32(retPid, 0);
								return errnoChild;
							}
							writeU32(retStatus, exitedChildren.get(childPid) || 0);
							writeU32(retPid, childPid);
							exitedChildren.delete(childPid);
							return errnoSuccess;
						},
						fd_dup(fd, retNewFd) {
							const descriptor = fd >>> 0;
							const handle = lookupSyntheticFd(descriptor) || (descriptor <= 2
								? { kind: "stdio", targetFd: descriptor, open: true }
								: null);
							if (!handle) return writeU32(retNewFd, fd);
							const cloned = cloneSyntheticHandle(handle);
							if (!cloned) return errnoBadf;
							return writeU32(retNewFd, allocateSyntheticFd(cloned));
						},
						fd_dup2(oldFd, newFd) {
							if (oldFd === newFd) return errnoSuccess;
							const handle = lookupSyntheticFd(oldFd >>> 0);
							if (!handle) return oldFd <= 2 && newFd <= 2 ? errnoSuccess : errnoBadf;
							const cloned = cloneSyntheticHandle(handle);
							if (!cloned) return errnoBadf;
							replaceSyntheticFd(newFd >>> 0, cloned);
							return errnoSuccess;
						},
						fd_pipe(retReadFd, retWriteFd) {
							const pipe = {
								chunks: [],
								consumers: new Map(),
								producers: new Map(),
								readHandleCount: 1,
								writeHandleCount: 1,
							};
							const readFd = allocateSyntheticFd({ kind: "pipe-read", pipe, open: true, onClose: onPipeHandleClose });
							const writeFd = allocateSyntheticFd({ kind: "pipe-write", pipe, open: true, onClose: onPipeHandleClose });
							writeU32(retReadFd, readFd);
							writeU32(retWriteFd, writeFd);
							return errnoSuccess;
						},
						proc_getpid(retPid) { return writeU32(retPid, 1); },
						proc_getppid(retPid) { return writeU32(retPid, 0); },
						proc_kill() { return errnoNosys; },
						sleep_ms(milliseconds) {
							Atomics.wait(wait, 0, 0, milliseconds >>> 0);
							return errnoSuccess;
						},
						pty_open() { return errnoNosys; },
						proc_sigaction() { return errnoSuccess; },
					},
				},
			};
			return host;
		}
		module.exports = { createWasiCommandHost };
		module.exports.default = module.exports;
	`,
      os: `
		const virtualOs = globalThis.__agentOSVirtualOs || {};
		const stringValue = (value, fallback) =>
			typeof value === "string" && value.length > 0 ? value : fallback;
		const platform = stringValue(virtualOs.platform, "linux");
		const arch = stringValue(virtualOs.arch, "x64");
		const homedir = stringValue(virtualOs.homedir, "/home/user");
		const tmpdir = stringValue(virtualOs.tmpdir, "/tmp");
		const username = stringValue(virtualOs.user, "user");
		const shell = stringValue(virtualOs.shell, "/bin/sh");
		const positiveInteger = (value, fallback) =>
			Number.isSafeInteger(value) && value > 0 ? value : fallback;
		const nonNegativeInteger = (value, fallback) =>
			Number.isSafeInteger(value) && value >= 0 ? value : fallback;
		const cpuCount = positiveInteger(virtualOs.cpuCount, 1);
		const totalmem = positiveInteger(virtualOs.totalmem, 1024 * 1024 * 1024);
		const freemem = Math.min(
			positiveInteger(virtualOs.freemem, 512 * 1024 * 1024),
			totalmem,
		);
		const uid = nonNegativeInteger(virtualOs.uid, 1000);
		const gid = nonNegativeInteger(virtualOs.gid, 1000);
		const cpuInfo = () => ({
			model: stringValue(virtualOs.cpuModel, "secure-exec virtual CPU"),
			speed: 0,
			times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 },
		});
		module.exports = {
			EOL: "\\n",
			arch: () => arch,
			cpus: () => Array.from({ length: cpuCount }, cpuInfo),
			endianness: () => "LE",
			freemem: () => freemem,
			getPriority: () => 0,
			homedir: () => homedir,
			hostname: () => stringValue(virtualOs.hostname, "secure-exec"),
			loadavg: () => [0, 0, 0],
			machine: () => stringValue(virtualOs.machine, "x86_64"),
			networkInterfaces: () => ({}),
			platform: () => platform,
			release: () => stringValue(virtualOs.release, "6.8.0-secure-exec"),
			tmpdir: () => tmpdir,
			totalmem: () => totalmem,
			type: () => stringValue(virtualOs.type, platform === "win32" ? "Windows_NT" : "Linux"),
			uptime: () => 0,
			userInfo: () => ({ username, uid, gid, shell, homedir }),
			version: () => stringValue(virtualOs.version, "#1 SMP PREEMPT_DYNAMIC secure-exec"),
		};
	`,
      "node:os": "module.exports = require('os');"
    };
  }
});

// ../../../agent-os/packages/browser/dist/sync-bridge.js
var SYNC_BRIDGE_SIGNAL_BYTES, SYNC_BRIDGE_DEFAULT_DATA_BYTES, SYNC_BRIDGE_MIN_DATA_BYTES, BROWSER_SYNC_BRIDGE_OPERATIONS, BROWSER_SYNC_BRIDGE_OPERATION_SET;
var init_sync_bridge = __esm({
  "../../../agent-os/packages/browser/dist/sync-bridge.js"() {
    "use strict";
    SYNC_BRIDGE_SIGNAL_BYTES = 4 * Int32Array.BYTES_PER_ELEMENT;
    SYNC_BRIDGE_DEFAULT_DATA_BYTES = 16 * 1024 * 1024;
    SYNC_BRIDGE_MIN_DATA_BYTES = 64 * 1024;
    BROWSER_SYNC_BRIDGE_OPERATIONS = [
      "fs.readFile",
      "fs.writeFile",
      "fs.readFileBinary",
      "fs.writeFileBinary",
      "fs.pread",
      "fs.pwrite",
      "fs.readDir",
      "fs.createDir",
      "fs.mkdir",
      "fs.rmdir",
      "fs.exists",
      "fs.stat",
      "fs.lstat",
      "fs.unlink",
      "fs.rename",
      "fs.realpath",
      "fs.readlink",
      "fs.symlink",
      "fs.link",
      "fs.chmod",
      "fs.truncate",
      "module.resolve",
      "module.loadFile",
      "module.format",
      "module.batchResolve",
      "child_process.spawn",
      "child_process.poll",
      "child_process.write_stdin",
      "child_process.close_stdin",
      "child_process.kill",
      "child_process.spawn_sync",
      "process.signal_state",
      "network.fetch",
      "dgram.create",
      "dgram.bind",
      "dgram.recv",
      "dgram.send",
      "dgram.close",
      "dgram.address",
      "dgram.setBufferSize",
      "dgram.getBufferSize",
      "pty.open",
      "pty.read",
      "pty.write",
      "pty.close",
      "pty.resize",
      "pty.setForegroundPgid",
      "pty.tcgetattr",
      "pty.tcsetattr"
    ];
    BROWSER_SYNC_BRIDGE_OPERATION_SET = new Set(BROWSER_SYNC_BRIDGE_OPERATIONS);
  }
});

// ../../../agent-os/packages/browser/dist/converged-base64.js
var init_converged_base64 = __esm({
  "../../../agent-os/packages/browser/dist/converged-base64.js"() {
    "use strict";
  }
});

// ../../../agent-os/packages/browser/dist/converged-fs-bridge.js
var init_converged_fs_bridge = __esm({
  "../../../agent-os/packages/browser/dist/converged-fs-bridge.js"() {
    "use strict";
    init_converged_base64();
    init_sync_bridge();
  }
});

// ../../../agent-os/packages/browser/dist/converged-net-bridge.js
var CONVERGED_NET_BRIDGE_OPERATIONS, CONVERGED_NET_BRIDGE_OPERATION_SET;
var init_converged_net_bridge = __esm({
  "../../../agent-os/packages/browser/dist/converged-net-bridge.js"() {
    "use strict";
    init_converged_base64();
    init_sync_bridge();
    CONVERGED_NET_BRIDGE_OPERATIONS = [
      "net.connect",
      "net.listen",
      "net.accept",
      "net.read",
      "net.write",
      "net.poll",
      "net.shutdown",
      "net.close",
      "net.udp_bind",
      "net.send_to",
      "net.recv_from",
      "dns.lookup"
    ];
    CONVERGED_NET_BRIDGE_OPERATION_SET = new Set(CONVERGED_NET_BRIDGE_OPERATIONS);
  }
});

// ../../../agent-os/packages/browser/dist/converged-dgram-bridge.js
var init_converged_dgram_bridge = __esm({
  "../../../agent-os/packages/browser/dist/converged-dgram-bridge.js"() {
    "use strict";
    init_converged_base64();
    init_sync_bridge();
  }
});

// ../../../agent-os/packages/browser/dist/converged-pty-bridge.js
var CONVERGED_PTY_BRIDGE_OPERATIONS, CONVERGED_PTY_BRIDGE_OPERATION_SET;
var init_converged_pty_bridge = __esm({
  "../../../agent-os/packages/browser/dist/converged-pty-bridge.js"() {
    "use strict";
    init_converged_base64();
    init_sync_bridge();
    CONVERGED_PTY_BRIDGE_OPERATIONS = [
      "pty.open",
      "pty.read",
      "pty.write",
      "pty.close",
      "pty.resize",
      "pty.setForegroundPgid",
      "pty.tcgetattr",
      "pty.tcsetattr"
    ];
    CONVERGED_PTY_BRIDGE_OPERATION_SET = new Set(CONVERGED_PTY_BRIDGE_OPERATIONS);
  }
});

// ../../../agent-os/packages/browser/dist/converged-sync-bridge-handler.js
var init_converged_sync_bridge_handler = __esm({
  "../../../agent-os/packages/browser/dist/converged-sync-bridge-handler.js"() {
    "use strict";
    init_protocol_frames();
    init_converged_fs_bridge();
    init_converged_net_bridge();
    init_converged_dgram_bridge();
    init_converged_pty_bridge();
    init_sync_bridge();
  }
});

// tests/browser-wasm/async-infer.entry.ts
var import_buffer = __toESM(require_buffer(), 1);

// tests/browser-wasm/async-harness.ts
init_protocol_frames();
init_protocol_schema();

// ../../../agent-os/packages/browser/dist/driver.js
init_encoding();
init_runtime();
var BROWSER_SYSTEM_DRIVER_OPTIONS = Symbol.for("secure-exec.browserSystemDriverOptions");
var NATIVE_FETCH = typeof globalThis !== "undefined" && typeof globalThis.fetch === "function" ? globalThis.fetch.bind(globalThis) : void 0;

// ../../../agent-os/packages/browser/dist/index.js
init_os_filesystem();
init_runtime();

// ../../../agent-os/packages/browser/dist/child-process-bridge.js
init_encoding();

// ../../../agent-os/packages/browser/dist/runtime-driver.js
init_encoding();
init_runtime();
init_signals();
init_sync_bridge();

// ../../../agent-os/packages/browser/dist/default-sidecar.js
var WASM_MODULE_URL = new URL("./sidecar-wasm-web/agentos_native_sidecar_browser.js", import.meta.url);
var WASM_BINARY_URL = new URL("./sidecar-wasm-web/agentos_native_sidecar_browser_bg.wasm", import.meta.url);

// ../../../agent-os/packages/browser/dist/sab-ring.js
var HEAD_INDEX = 0;
var TAIL_INDEX = 1;
var HEADER_I32 = 4;
var HEADER_BYTES = HEADER_I32 * Int32Array.BYTES_PER_ELEMENT;
var LEN_PREFIX_BYTES = Int32Array.BYTES_PER_ELEMENT;
function sabRingByteLength(layout) {
  return HEADER_BYTES + layout.slotCount * layout.slotBytes;
}
function sabRingMaxFrameBytes(slotBytes) {
  return slotBytes - LEN_PREFIX_BYTES;
}
var SabRing = class {
  control;
  bytes;
  slotCount;
  slotBytes;
  maxFrameBytes;
  constructor(sab, layout) {
    if (layout.slotCount <= 0 || (layout.slotCount & layout.slotCount - 1) !== 0) {
      throw new Error("SabRing slotCount must be a positive power of two");
    }
    if (layout.slotBytes <= LEN_PREFIX_BYTES) {
      throw new Error("SabRing slotBytes must exceed the length prefix");
    }
    if (sab.byteLength < sabRingByteLength(layout)) {
      throw new Error("SabRing SharedArrayBuffer too small for layout");
    }
    this.control = new Int32Array(sab, 0, HEADER_I32);
    this.bytes = new Uint8Array(sab, HEADER_BYTES, layout.slotCount * layout.slotBytes);
    this.slotCount = layout.slotCount;
    this.slotBytes = layout.slotBytes;
    this.maxFrameBytes = sabRingMaxFrameBytes(layout.slotBytes);
  }
  get capacityFrames() {
    return this.slotCount;
  }
  get maxFrame() {
    return this.maxFrameBytes;
  }
  /** Producer side: enqueue one frame. Returns false if the ring is full
   * (backpressure) — the UNTRUSTED producer may then block/retry; the TCB
   * consumer must never block on a full ring (§4/F7). Throws only on a local
   * programming error (frame too large for the slot). */
  tryWrite(frame) {
    if (frame.byteLength > this.maxFrameBytes) {
      throw new Error(`SabRing frame ${frame.byteLength} exceeds slot capacity ${this.maxFrameBytes}`);
    }
    const head = Atomics.load(this.control, HEAD_INDEX);
    const tail = Atomics.load(this.control, TAIL_INDEX);
    if (tail - head >= this.slotCount)
      return false;
    const slot = tail % this.slotCount * this.slotBytes;
    this.bytes[slot] = frame.byteLength & 255;
    this.bytes[slot + 1] = frame.byteLength >>> 8 & 255;
    this.bytes[slot + 2] = frame.byteLength >>> 16 & 255;
    this.bytes[slot + 3] = frame.byteLength >>> 24 & 255;
    this.bytes.set(frame, slot + LEN_PREFIX_BYTES);
    Atomics.store(this.control, TAIL_INDEX, tail + 1);
    return true;
  }
  /** Consumer side: dequeue one frame as a fresh kernel-private copy, or null if
   * empty. Validates the length as HOSTILE input (§4/F3): a length outside
   * [0, maxFrame] throws (the caller must kill that execution, §7), never reads OOB.
   * Copy-then-validate: we snapshot the length, bound-check it, then copy exactly
   * that many bytes — no re-read of shared memory after the check. */
  tryRead() {
    const tail = Atomics.load(this.control, TAIL_INDEX);
    const head = Atomics.load(this.control, HEAD_INDEX);
    if (head === tail)
      return null;
    const slot = head % this.slotCount * this.slotBytes;
    const len = (this.bytes[slot] | this.bytes[slot + 1] << 8 | this.bytes[slot + 2] << 16 | this.bytes[slot + 3] << 24) >>> 0;
    if (len > this.maxFrameBytes) {
      throw new SabRingProtocolError(`frame length ${len} exceeds slot capacity ${this.maxFrameBytes}`);
    }
    const out = new Uint8Array(len);
    out.set(this.bytes.subarray(slot + LEN_PREFIX_BYTES, slot + LEN_PREFIX_BYTES + len));
    Atomics.store(this.control, HEAD_INDEX, head + 1);
    return out;
  }
  /** True if at least one frame is queued (consumer view). */
  hasPending() {
    return Atomics.load(this.control, HEAD_INDEX) !== Atomics.load(this.control, TAIL_INDEX);
  }
};
var SabRingProtocolError = class extends Error {
  constructor(message) {
    super(`SAB ring protocol violation: ${message}`);
    this.name = "SabRingProtocolError";
  }
};

// ../../../agent-os/packages/browser/dist/sab-reactor.js
var REACTOR_CONTROL_BYTES = 1 * Int32Array.BYTES_PER_ELEMENT;
var DEFERRED = Symbol("syscall-deferred");
function encodeSyscallCompletion(executionId, result) {
  const id = new TextEncoder().encode(executionId);
  const out = new Uint8Array(1 + id.byteLength + result.byteLength);
  out[0] = id.byteLength;
  out.set(id, 1);
  out.set(result, 1 + id.byteLength);
  return out;
}

// ../../../agent-os/packages/browser/dist/index.js
init_converged_sync_bridge_handler();

// src/chrome-llm-adapter.ts
function contentToText(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content.map(
      (part) => part && typeof part === "object" && "text" in part ? String(part.text) : typeof part === "string" ? part : ""
    ).join("");
  }
  return "";
}
function chatRequestToPrompt(request) {
  if (typeof request.prompt === "string") return request.prompt;
  const lines = [];
  if (request.system) lines.push(`system: ${request.system}`);
  for (const message of request.messages ?? []) {
    lines.push(`${message.role}: ${contentToText(message.content)}`);
  }
  return lines.join("\n");
}
async function handleChatCompletion(requestBody, session) {
  let request;
  try {
    request = JSON.parse(requestBody);
  } catch {
    return JSON.stringify({ error: { type: "invalid_request", message: "invalid JSON body" } });
  }
  const text = await session.prompt(chatRequestToPrompt(request));
  return JSON.stringify({
    id: "chatcmpl-chrome-local",
    object: "chat.completion",
    model: request.model ?? "chrome-local",
    choices: [
      {
        index: 0,
        message: { role: "assistant", content: text },
        finish_reason: "stop"
      }
    ]
  });
}

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/imports/dev.js
var DEV2 = false;

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/assert.js
var V8Error2 = Error;
function assert2(test, message = "") {
  if (!test) {
    const e = new AssertionError2(message);
    V8Error2.captureStackTrace?.(e, assert2);
    throw e;
  }
}
var AssertionError2 = class extends Error {
  constructor() {
    super(...arguments);
    this.name = "AssertionError";
  }
};

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/validator.js
function isI322(val) {
  return val === (val | 0);
}
function isU82(val) {
  return val === (val & 255);
}
function isU322(val) {
  return val === val >>> 0;
}
function isU64Safe2(val) {
  return Number.isSafeInteger(val) && val >= 0;
}

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/util/constants.js
var TEXT_DECODER_THRESHOLD2 = 256;
var TEXT_ENCODER_THRESHOLD2 = 256;
var INT_SAFE_MAX_BYTE_COUNT2 = 8;
var UINT_SAFE32_MAX_BYTE_COUNT2 = 5;
var INVALID_UTF8_STRING2 = "invalid UTF-8 string";
var NON_CANONICAL_REPRESENTATION2 = "must be canonical";
var TOO_LARGE_BUFFER2 = "too large buffer";
var TOO_LARGE_NUMBER2 = "too large number";

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/bare-error.js
var BareError2 = class extends Error {
  constructor(offset, issue, opts) {
    super(`(byte:${offset}) ${issue}`);
    this.name = "BareError";
    this.issue = issue;
    this.offset = offset;
    this.cause = opts?.cause;
  }
};

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/byte-cursor.js
var ByteCursor2 = class {
  /**
   * @throws {BareError} Buffer exceeds `config.maxBufferLength`
   */
  constructor(bytes, config) {
    this.offset = 0;
    if (bytes.length > config.maxBufferLength) {
      throw new BareError2(0, TOO_LARGE_BUFFER2);
    }
    this.bytes = bytes;
    this.config = config;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
  }
};
function check2(bc, min) {
  if (DEV2) {
    assert2(isU322(min));
  }
  if (bc.offset + min > bc.bytes.length) {
    throw new BareError2(bc.offset, "missing bytes");
  }
}
function reserve2(bc, min) {
  if (DEV2) {
    assert2(isU322(min));
  }
  const minLen = bc.offset + min | 0;
  if (minLen > bc.bytes.length) {
    grow2(bc, minLen);
  }
}
function grow2(bc, minLen) {
  if (minLen > bc.config.maxBufferLength) {
    throw new BareError2(0, TOO_LARGE_BUFFER2);
  }
  const buffer = bc.bytes.buffer;
  let newBytes;
  if (isEs2024ArrayBufferLike2(buffer) && // Make sure that the view covers the end of the buffer.
  // If it is not the case, this indicates that the user don't want
  // to override the trailing bytes.
  bc.bytes.byteOffset + bc.bytes.byteLength === buffer.byteLength && bc.bytes.byteLength + minLen <= buffer.maxByteLength) {
    const newLen = Math.min(minLen << 1, bc.config.maxBufferLength, buffer.maxByteLength);
    if (buffer instanceof ArrayBuffer) {
      buffer.resize(newLen);
    } else {
      buffer.grow(newLen);
    }
    newBytes = new Uint8Array(buffer, bc.bytes.byteOffset, newLen);
  } else {
    const newLen = Math.min(minLen << 1, bc.config.maxBufferLength);
    newBytes = new Uint8Array(newLen);
    newBytes.set(bc.bytes);
  }
  bc.bytes = newBytes;
  bc.view = new DataView(newBytes.buffer);
}
function isEs2024ArrayBufferLike2(buffer) {
  return "maxByteLength" in buffer;
}

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/fixed-primitive.js
function readBool2(bc) {
  const val = readU82(bc);
  if (val > 1) {
    bc.offset--;
    throw new BareError2(bc.offset, "a bool must be equal to 0 or 1");
  }
  return val > 0;
}
function writeBool2(bc, x) {
  writeU82(bc, x ? 1 : 0);
}
function readI322(bc) {
  check2(bc, 4);
  const result = bc.view.getInt32(bc.offset, true);
  bc.offset += 4;
  return result;
}
function writeI322(bc, x) {
  if (DEV2) {
    assert2(isI322(x), TOO_LARGE_NUMBER2);
  }
  reserve2(bc, 4);
  bc.view.setInt32(bc.offset, x, true);
  bc.offset += 4;
}
function readU82(bc) {
  check2(bc, 1);
  return bc.bytes[bc.offset++];
}
function writeU82(bc, x) {
  if (DEV2) {
    assert2(isU82(x), TOO_LARGE_NUMBER2);
  }
  reserve2(bc, 1);
  bc.bytes[bc.offset++] = x;
}
function readU322(bc) {
  check2(bc, 4);
  const result = bc.view.getUint32(bc.offset, true);
  bc.offset += 4;
  return result;
}

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/uint.js
function readUintSafe322(bc) {
  let result = readU82(bc);
  if (result >= 128) {
    result &= 127;
    let shift = 7;
    let byteCount = 1;
    let byte;
    do {
      byte = readU82(bc);
      result += (byte & 127) << shift >>> 0;
      shift += 7;
      byteCount++;
    } while (byte >= 128 && byteCount < UINT_SAFE32_MAX_BYTE_COUNT2);
    if (byte === 0) {
      bc.offset -= byteCount - 1;
      throw new BareError2(bc.offset - byteCount + 1, NON_CANONICAL_REPRESENTATION2);
    }
    if (byteCount === UINT_SAFE32_MAX_BYTE_COUNT2 && byte > 15) {
      bc.offset -= byteCount - 1;
      throw new BareError2(bc.offset, TOO_LARGE_NUMBER2);
    }
  }
  return result;
}
function writeUintSafe322(bc, x) {
  if (DEV2) {
    assert2(isU322(x), TOO_LARGE_NUMBER2);
  }
  let zigZag = x >>> 0;
  while (zigZag >= 128) {
    writeU82(bc, 128 | zigZag & 127);
    zigZag >>>= 7;
  }
  writeU82(bc, zigZag);
}
function readUintSafe2(bc) {
  let result = readU82(bc);
  if (result >= 128) {
    result &= 127;
    let shiftMul = (
      /* 2**7 */
      128
    );
    let byteCount = 1;
    let byte;
    do {
      byte = readU82(bc);
      result += (byte & 127) * shiftMul;
      shiftMul *= /* 2**7 */
      128;
      byteCount++;
    } while (byte >= 128 && byteCount < INT_SAFE_MAX_BYTE_COUNT2);
    if (byte === 0) {
      bc.offset -= byteCount - 1;
      throw new BareError2(bc.offset - byteCount + 1, NON_CANONICAL_REPRESENTATION2);
    }
    if (byteCount === INT_SAFE_MAX_BYTE_COUNT2 && byte > 15) {
      bc.offset -= byteCount - 1;
      throw new BareError2(bc.offset, TOO_LARGE_NUMBER2);
    }
  }
  return result;
}
function writeUintSafe2(bc, x) {
  if (DEV2) {
    assert2(isU64Safe2(x), TOO_LARGE_NUMBER2);
  }
  let byteCount = 1;
  let zigZag = x;
  while (zigZag >= 128 && byteCount < INT_SAFE_MAX_BYTE_COUNT2) {
    writeU82(bc, 128 | zigZag & 127);
    zigZag = Math.floor(zigZag / /* 2**7 */
    128);
    byteCount++;
  }
  if (byteCount === INT_SAFE_MAX_BYTE_COUNT2) {
    zigZag &= 15;
  }
  writeU82(bc, zigZag);
}

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/u8-array.js
function writeU8Array2(bc, x) {
  writeUintSafe322(bc, x.length);
  writeU8FixedArray2(bc, x);
}
function writeU8FixedArray2(bc, x) {
  const len = x.length;
  if (len > 0) {
    reserve2(bc, len);
    bc.bytes.set(x, bc.offset);
    bc.offset += len;
  }
}
function readUnsafeU8FixedArray2(bc, len) {
  if (DEV2) {
    assert2(isU322(len));
  }
  check2(bc, len);
  const offset = bc.offset;
  bc.offset += len;
  return bc.bytes.subarray(offset, offset + len);
}

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/data.js
function writeData2(bc, x) {
  writeU8Array2(bc, new Uint8Array(x));
}

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/codec/string.js
function readString2(bc) {
  return readFixedString2(bc, readUintSafe322(bc));
}
function writeString2(bc, x) {
  if (x.length < TEXT_ENCODER_THRESHOLD2) {
    const byteLen = utf8ByteLength2(x);
    writeUintSafe322(bc, byteLen);
    reserve2(bc, byteLen);
    writeUtf8Js2(bc, x);
  } else {
    const strBytes = UTF8_ENCODER2.encode(x);
    writeUintSafe322(bc, strBytes.length);
    writeU8FixedArray2(bc, strBytes);
  }
}
function readFixedString2(bc, byteLen) {
  if (DEV2) {
    assert2(isU322(byteLen));
  }
  if (byteLen < TEXT_DECODER_THRESHOLD2) {
    return readUtf8Js2(bc, byteLen);
  }
  try {
    return UTF8_DECODER2.decode(readUnsafeU8FixedArray2(bc, byteLen));
  } catch (_cause) {
    throw new BareError2(bc.offset, INVALID_UTF8_STRING2);
  }
}
function readUtf8Js2(bc, byteLen) {
  check2(bc, byteLen);
  let result = "";
  const bytes = bc.bytes;
  let offset = bc.offset;
  const upperOffset = offset + byteLen;
  while (offset < upperOffset) {
    let codePoint = bytes[offset++];
    if (codePoint > 127) {
      let malformed = true;
      const byte1 = codePoint;
      if (offset < upperOffset && codePoint < 224) {
        const byte2 = bytes[offset++];
        codePoint = (byte1 & 31) << 6 | byte2 & 63;
        malformed = codePoint >> 7 === 0 || // non-canonical char
        byte1 >> 5 !== 6 || // invalid tag
        byte2 >> 6 !== 2;
      } else if (offset + 1 < upperOffset && codePoint < 240) {
        const byte2 = bytes[offset++];
        const byte3 = bytes[offset++];
        codePoint = (byte1 & 15) << 12 | (byte2 & 63) << 6 | byte3 & 63;
        malformed = codePoint >> 11 === 0 || // non-canonical char or missing data
        codePoint >> 11 === 27 || // surrogate char (0xD800 <= codePoint <= 0xDFFF)
        byte1 >> 4 !== 14 || // invalid tag
        byte2 >> 6 !== 2 || // invalid tag
        byte3 >> 6 !== 2;
      } else if (offset + 2 < upperOffset) {
        const byte2 = bytes[offset++];
        const byte3 = bytes[offset++];
        const byte4 = bytes[offset++];
        codePoint = (byte1 & 7) << 18 | (byte2 & 63) << 12 | (byte3 & 63) << 6 | byte4 & 63;
        malformed = codePoint >> 16 === 0 || // non-canonical char or missing data
        codePoint > 1114111 || // too large code point
        byte1 >> 3 !== 30 || // invalid tag
        byte2 >> 6 !== 2 || // invalid tag
        byte3 >> 6 !== 2 || // invalid tag
        byte4 >> 6 !== 2;
      }
      if (malformed) {
        throw new BareError2(bc.offset, INVALID_UTF8_STRING2);
      }
    }
    result += String.fromCodePoint(codePoint);
  }
  bc.offset = offset;
  return result;
}
function writeUtf8Js2(bc, s) {
  const bytes = bc.bytes;
  let offset = bc.offset;
  let i = 0;
  while (i < s.length) {
    const codePoint = s.codePointAt(i++);
    if (codePoint < 128) {
      bytes[offset++] = codePoint;
    } else {
      if (codePoint < 2048) {
        bytes[offset++] = 192 | codePoint >> 6;
      } else {
        if (codePoint < 65536) {
          bytes[offset++] = 224 | codePoint >> 12;
        } else {
          bytes[offset++] = 240 | codePoint >> 18;
          bytes[offset++] = 128 | codePoint >> 12 & 63;
          i++;
        }
        bytes[offset++] = 128 | codePoint >> 6 & 63;
      }
      bytes[offset++] = 128 | codePoint & 63;
    }
  }
  bc.offset = offset;
}
function utf8ByteLength2(s) {
  let result = s.length;
  for (let i = 0; i < s.length; i++) {
    const codePoint = s.codePointAt(i);
    if (codePoint > 127) {
      result++;
      if (codePoint > 2047) {
        result++;
        if (codePoint > 65535) {
          i++;
        }
      }
    }
  }
  return result;
}
var UTF8_DECODER2 = /* @__PURE__ */ new TextDecoder("utf-8", { fatal: true });
var UTF8_ENCODER2 = /* @__PURE__ */ new TextEncoder();

// ../../node_modules/.pnpm/@rivetkit+bare-ts@0.6.2/node_modules/@rivetkit/bare-ts/dist/core/config.js
function Config2({ initialBufferLength = 1024, maxBufferLength = 1024 * 1024 * 32 }) {
  if (DEV2) {
    assert2(isU322(initialBufferLength), TOO_LARGE_NUMBER2);
    assert2(isU322(maxBufferLength), TOO_LARGE_NUMBER2);
    assert2(initialBufferLength <= maxBufferLength, "initialBufferLength must be lower than or equal to maxBufferLength");
  }
  return {
    initialBufferLength,
    maxBufferLength
  };
}

// ../core/src/sidecar/agentos-protocol.ts
var DEFAULT_CONFIG2 = /* @__PURE__ */ Config2({});
function readJsonUtf82(bc) {
  return readString2(bc);
}
function writeJsonUtf82(bc, x) {
  writeString2(bc, x);
}
function writeAcpRuntimeKind(bc, x) {
  switch (x) {
    case "JavaScript" /* JavaScript */: {
      writeU82(bc, 0);
      break;
    }
    case "Python" /* Python */: {
      writeU82(bc, 1);
      break;
    }
    case "WebAssembly" /* WebAssembly */: {
      writeU82(bc, 2);
      break;
    }
  }
}
function write02(bc, x) {
  writeUintSafe2(bc, x.length);
  for (let i = 0; i < x.length; i++) {
    writeString2(bc, x[i]);
  }
}
function write110(bc, x) {
  writeUintSafe2(bc, x.size);
  for (const kv of x) {
    writeString2(bc, kv[0]);
    writeString2(bc, kv[1]);
  }
}
function write210(bc, x) {
  writeBool2(bc, x != null);
  if (x != null) {
    writeString2(bc, x);
  }
}
function writeAcpCreateSessionRequest(bc, x) {
  writeString2(bc, x.agentType);
  writeAcpRuntimeKind(bc, x.runtime);
  writeString2(bc, x.adapterEntrypoint);
  writeString2(bc, x.cwd);
  write02(bc, x.args);
  write110(bc, x.env);
  writeI322(bc, x.protocolVersion);
  writeJsonUtf82(bc, x.clientCapabilities);
  writeJsonUtf82(bc, x.mcpServers);
  writeBool2(bc, x.skipOsInstructions);
  write210(bc, x.additionalInstructions);
}
function read32(bc) {
  return readBool2(bc) ? readJsonUtf82(bc) : null;
}
function write32(bc, x) {
  writeBool2(bc, x != null);
  if (x != null) {
    writeJsonUtf82(bc, x);
  }
}
function writeAcpSessionRequest(bc, x) {
  writeString2(bc, x.sessionId);
  writeString2(bc, x.method);
  write32(bc, x.params);
}
function writeAcpGetSessionStateRequest(bc, x) {
  writeString2(bc, x.sessionId);
}
function writeAcpCloseSessionRequest(bc, x) {
  writeString2(bc, x.sessionId);
}
function writeAcpResumeSessionRequest(bc, x) {
  writeString2(bc, x.sessionId);
  writeString2(bc, x.agentType);
  write210(bc, x.transcriptPath);
  writeString2(bc, x.cwd);
  write110(bc, x.env);
}
function writeAcpDeliverAgentOutputRequest(bc, x) {
  writeString2(bc, x.processId);
  writeData2(bc, x.chunk);
}
function writeAcpRequest(bc, x) {
  switch (x.tag) {
    case "AcpCreateSessionRequest": {
      writeU82(bc, 0);
      writeAcpCreateSessionRequest(bc, x.val);
      break;
    }
    case "AcpSessionRequest": {
      writeU82(bc, 1);
      writeAcpSessionRequest(bc, x.val);
      break;
    }
    case "AcpGetSessionStateRequest": {
      writeU82(bc, 2);
      writeAcpGetSessionStateRequest(bc, x.val);
      break;
    }
    case "AcpCloseSessionRequest": {
      writeU82(bc, 3);
      writeAcpCloseSessionRequest(bc, x.val);
      break;
    }
    case "AcpResumeSessionRequest": {
      writeU82(bc, 4);
      writeAcpResumeSessionRequest(bc, x.val);
      break;
    }
    case "AcpDeliverAgentOutputRequest": {
      writeU82(bc, 5);
      writeAcpDeliverAgentOutputRequest(bc, x.val);
      break;
    }
  }
}
function encodeAcpRequest(x, config) {
  const fullConfig = config != null ? Config2(config) : DEFAULT_CONFIG2;
  const bc = new ByteCursor2(
    new Uint8Array(fullConfig.initialBufferLength),
    fullConfig
  );
  writeAcpRequest(bc, x);
  return new Uint8Array(bc.view.buffer, bc.view.byteOffset, bc.offset);
}
function read42(bc) {
  return readBool2(bc) ? readU322(bc) : null;
}
function read5(bc) {
  const len = readUintSafe2(bc);
  if (len === 0) {
    return [];
  }
  const result = [readJsonUtf82(bc)];
  for (let i = 1; i < len; i++) {
    result[i] = readJsonUtf82(bc);
  }
  return result;
}
function readAcpSessionCreatedResponse(bc) {
  return {
    sessionId: readString2(bc),
    pid: read42(bc),
    modes: read32(bc),
    configOptions: read5(bc),
    agentCapabilities: read32(bc),
    agentInfo: read32(bc)
  };
}
function readAcpSessionRpcResponse(bc) {
  return {
    sessionId: readString2(bc),
    response: readJsonUtf82(bc)
  };
}
function read62(bc) {
  return readBool2(bc) ? readI322(bc) : null;
}
function readAcpSessionStateResponse(bc) {
  return {
    sessionId: readString2(bc),
    agentType: readString2(bc),
    processId: readString2(bc),
    pid: read42(bc),
    closed: readBool2(bc),
    exitCode: read62(bc),
    modes: read32(bc),
    configOptions: read5(bc),
    agentCapabilities: read32(bc),
    agentInfo: read32(bc)
  };
}
function readAcpSessionClosedResponse(bc) {
  return {
    sessionId: readString2(bc)
  };
}
function readAcpSessionResumedResponse(bc) {
  return {
    sessionId: readString2(bc),
    mode: readString2(bc)
  };
}
function readAcpErrorResponse(bc) {
  return {
    code: readString2(bc),
    message: readString2(bc)
  };
}
function readAcpPendingResponse(bc) {
  return {
    processId: readString2(bc)
  };
}
function readAcpResponse(bc) {
  const offset = bc.offset;
  const tag = readU82(bc);
  switch (tag) {
    case 0:
      return { tag: "AcpSessionCreatedResponse", val: readAcpSessionCreatedResponse(bc) };
    case 1:
      return { tag: "AcpSessionRpcResponse", val: readAcpSessionRpcResponse(bc) };
    case 2:
      return { tag: "AcpSessionStateResponse", val: readAcpSessionStateResponse(bc) };
    case 3:
      return { tag: "AcpSessionClosedResponse", val: readAcpSessionClosedResponse(bc) };
    case 4:
      return { tag: "AcpSessionResumedResponse", val: readAcpSessionResumedResponse(bc) };
    case 5:
      return { tag: "AcpErrorResponse", val: readAcpErrorResponse(bc) };
    case 6:
      return { tag: "AcpPendingResponse", val: readAcpPendingResponse(bc) };
    default: {
      bc.offset = offset;
      throw new BareError2(offset, "invalid tag");
    }
  }
}
function decodeAcpResponse(bytes) {
  const bc = new ByteCursor2(bytes, DEFAULT_CONFIG2);
  const result = readAcpResponse(bc);
  if (bc.offset < bc.view.byteLength) {
    throw new BareError2(bc.offset, "remaining bytes");
  }
  return result;
}

// tests/browser-wasm/async-harness.ts
var ACP_NS = "dev.rivet.agent-os.acp";
var LAYOUT = { slotCount: 64, slotBytes: 4096 };
var nextRequestId = 1;
var KernelWorkerRelay = class {
  /** @param inferenceSession the on-device model the host-callback drives (a mock
   * sentinel for the CI gate, the real `LanguageModel` for the Nano smoke). When
   * null, a `host-inference` callback errors (no agent should issue one). */
  constructor(url, inferenceSession = null) {
    this.inferenceSession = inferenceSession;
    this.worker = new Worker(url, { type: "module" });
    this.worker.onmessage = (e) => this.onMessage(e);
  }
  worker;
  id = 1;
  pending = /* @__PURE__ */ new Map();
  agents = /* @__PURE__ */ new Map();
  // Completion channel for DEFERRED inference syscalls; populated from the kernel's
  // `booted` message. Null until boot resolves.
  completion = null;
  control = null;
  /** Direct main-thread <-> agent-worker channel for interactive guests (the PTY
   * terminal). Agent workers are spawned HERE, so the host can postMessage them
   * straight (out-of-band of the SAB/ACP path) for live keystroke/output streaming.
   * Set before create_session so the first agent message is observed. */
  onAgentMessage = null;
  /** Execution id of the most recently spawned agent worker. */
  lastAgentExecutionId = null;
  onMessage(e) {
    const m = e.data;
    if (m.type === "spawn-agent") {
      const s = m;
      const agent = new Worker(s.workerUrl, { type: "module" });
      agent.onmessage = (ev) => this.onAgentMessage?.(s.executionId, ev.data);
      agent.postMessage({ type: "init", upSab: s.upSab, downSab: s.downSab, controlSab: s.controlSab, layout: s.layout });
      this.agents.set(s.executionId, agent);
      this.lastAgentExecutionId = s.executionId;
      return;
    }
    if (m.type === "agent-stdin") {
      const s = m;
      this.agents.get(s.executionId)?.postMessage({ type: "stdin", chunk: s.chunk });
      return;
    }
    if (m.type === "kill-agent") {
      this.agents.get(m.executionId)?.terminate();
      this.agents.delete(m.executionId);
      return;
    }
    if (m.type === "host-inference") {
      void this.completeInference(m);
      return;
    }
    const entry = this.pending.get(m.id);
    if (!entry) return;
    this.pending.delete(m.id);
    if (m.type === "error") entry.reject(new Error(String(m.message)));
    else entry.resolve(m);
  }
  // Run one async host-callback to the on-device model and deliver the reply to the
  // blocked guest via the kernel's completion channel. This is the single async hop
  // of the inference path (§6); everything else (the guest's net/fs syscalls) is
  // synchronous over the SAB.
  async completeInference(m) {
    if (!this.completion || !this.control) throw new Error("relay: completion channel not ready");
    const responseJson = this.inferenceSession ? await handleChatCompletion(m.body, this.inferenceSession) : JSON.stringify({ error: { type: "no_model", message: "no inference session bound" } });
    const result = new TextEncoder().encode(responseJson);
    if (!this.completion.tryWrite(encodeSyscallCompletion(m.executionId, result))) {
      throw new Error("relay: completion ring full");
    }
    Atomics.add(this.control, 0, 1);
    Atomics.notify(this.control, 0);
  }
  call(message, transfer = []) {
    const id = this.id++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.worker.postMessage({ ...message, id }, transfer);
    });
  }
  async boot() {
    const booted = await this.call({ type: "boot" });
    this.completion = new SabRing(booted.completionSab, LAYOUT);
    this.control = new Int32Array(booted.controlSab, 0, 1);
    return booted.sidecarId;
  }
  async pushFrame(frame, ownership) {
    return (await this.call({ type: "frame", frame, ownership }, [frame.buffer])).frame;
  }
  /** Post a message straight to a spawned agent worker (interactive PTY channel). */
  postToAgent(executionId, message) {
    this.agents.get(executionId)?.postMessage(message);
  }
  /** Start the kernel worker's continuous reactor drive so a long-lived interactive
   * agent's mid-life syscalls are serviced outside any pushFrame turn. */
  async driveTerminal() {
    await this.call({ type: "drive-terminal" });
  }
};
async function send(relay, ownership, payload) {
  const responseBytes = await relay.pushFrame(
    encodeBareProtocolFrame({
      frame_type: "request",
      schema: SIDECAR_PROTOCOL_SCHEMA,
      request_id: nextRequestId++,
      ownership,
      payload
    }),
    ownership
  );
  return decodeBareProtocolFrame(responseBytes).payload;
}
async function bootstrapVm(relay) {
  const authed = await send(
    relay,
    { scope: "connection", connection_id: "client-hint" },
    {
      type: "authenticate",
      client_name: "async-agent-test",
      auth_token: "",
      protocol_version: SIDECAR_PROTOCOL_SCHEMA.version,
      bridge_version: 1
    }
  );
  const connectionId = authed.connection_id;
  const opened = await send(
    relay,
    { scope: "connection", connection_id: connectionId },
    { type: "open_session", placement: { kind: "shared", pool: null }, metadata: {} }
  );
  const sessionId = opened.session_id;
  const created = await send(
    relay,
    { scope: "session", connection_id: connectionId, session_id: sessionId },
    {
      type: "create_vm",
      runtime: "java_script",
      config: {
        rootFilesystem: { mode: "ephemeral", disableDefaultBaseLayer: false, lowers: [], bootstrapEntries: [] },
        permissions: { fs: "allow", network: "allow", childProcess: "allow", process: "allow", env: "allow", binding: "allow" }
      }
    }
  );
  return { connectionId, sessionId, vmId: created.vm_id };
}
async function runSessionPromptGate(relay, opts) {
  const sidecarId = await relay.boot();
  const vm = await bootstrapVm(relay);
  const vmOwnership = { scope: "vm", connection_id: vm.connectionId, session_id: vm.sessionId, vm_id: vm.vmId };
  const createAcp = encodeAcpRequest({
    tag: "AcpCreateSessionRequest",
    val: {
      agentType: opts.agentType,
      runtime: "JavaScript",
      adapterEntrypoint: opts.adapterEntrypoint,
      cwd: "/workspace",
      args: [],
      env: /* @__PURE__ */ new Map(),
      protocolVersion: 1,
      clientCapabilities: "{}",
      mcpServers: "[]",
      skipOsInstructions: false,
      additionalInstructions: null
    }
  });
  const created = await send(relay, vmOwnership, {
    type: "ext",
    envelope: { namespace: ACP_NS, payload: createAcp }
  });
  const out = { sidecarId, payloadType: created.type };
  if (created.type === "ext" || created.type === "ext_result") {
    const env = created.envelope;
    const decoded = decodeAcpResponse(env.payload);
    out.acpTag = decoded.tag;
    out.sessionId = decoded.val?.sessionId;
  }
  if (out.acpTag !== "AcpSessionCreatedResponse" || !out.sessionId) return out;
  const promptAcp = encodeAcpRequest({
    tag: "AcpSessionRequest",
    val: {
      sessionId: out.sessionId,
      method: "session/prompt",
      params: JSON.stringify({ prompt: [{ type: "text", text: opts.promptText }] })
    }
  });
  const prompted = await send(relay, vmOwnership, {
    type: "ext",
    envelope: { namespace: ACP_NS, payload: promptAcp }
  });
  if (prompted.type === "ext" || prompted.type === "ext_result") {
    const env = prompted.envelope;
    const decoded = decodeAcpResponse(env.payload);
    if (decoded.tag === "AcpSessionRpcResponse" && decoded.val?.response) {
      const rpc = JSON.parse(decoded.val.response);
      out.promptContent = rpc.result?.content;
    }
  }
  return out;
}

// tests/browser-wasm/async-infer.entry.ts
globalThis.Buffer ??= import_buffer.Buffer;
var SENTINEL = "PONG_FROM_CHROME_LLM";
var mockModel = { prompt: async () => SENTINEL };
globalThis.__asyncInfer = {
  async run() {
    const relay = new KernelWorkerRelay("/async-kernel.worker.js", mockModel);
    return runSessionPromptGate(relay, {
      agentType: "async-infer",
      adapterEntrypoint: "/bin/async-infer-agent",
      promptText: "ping"
    });
  }
};
var status = document.getElementById("status");
if (status) status.textContent = "ready";
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

@rivetkit/bare-ts/imports/dev.js:
@rivetkit/bare-ts/dist/util/assert.js:
@rivetkit/bare-ts/dist/util/validator.js:
@rivetkit/bare-ts/dist/util/constants.js:
@rivetkit/bare-ts/dist/core/bare-error.js:
@rivetkit/bare-ts/dist/core/byte-cursor.js:
@rivetkit/bare-ts/dist/codec/fixed-primitive.js:
@rivetkit/bare-ts/dist/codec/u8-array.js:
@rivetkit/bare-ts/dist/codec/data.js:
@rivetkit/bare-ts/dist/codec/f32-array.js:
@rivetkit/bare-ts/dist/codec/f64-array.js:
@rivetkit/bare-ts/dist/codec/i8-array.js:
@rivetkit/bare-ts/dist/codec/i16-array.js:
@rivetkit/bare-ts/dist/codec/i32-array.js:
@rivetkit/bare-ts/dist/codec/i64-array.js:
@rivetkit/bare-ts/dist/codec/string.js:
@rivetkit/bare-ts/dist/codec/u8-clamped-array.js:
@rivetkit/bare-ts/dist/codec/u16-array.js:
@rivetkit/bare-ts/dist/codec/u32-array.js:
@rivetkit/bare-ts/dist/codec/u64-array.js:
@rivetkit/bare-ts/dist/core/config.js:
@rivetkit/bare-ts/dist/index.js:
@rivetkit/bare-ts/imports/dev.js:
@rivetkit/bare-ts/dist/util/assert.js:
@rivetkit/bare-ts/dist/util/validator.js:
@rivetkit/bare-ts/dist/util/constants.js:
@rivetkit/bare-ts/dist/core/bare-error.js:
@rivetkit/bare-ts/dist/core/byte-cursor.js:
@rivetkit/bare-ts/dist/codec/fixed-primitive.js:
@rivetkit/bare-ts/dist/codec/u8-array.js:
@rivetkit/bare-ts/dist/codec/data.js:
@rivetkit/bare-ts/dist/codec/string.js:
@rivetkit/bare-ts/dist/core/config.js:
  (*! Copyright (c) 2022 Victorien Elvinger *)
  (*! Licensed under the MIT License (https://mit-license.org/) *)
*/
