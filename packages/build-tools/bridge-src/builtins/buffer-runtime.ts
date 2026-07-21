import { BUFFER_MAX_LENGTH, BUFFER_MAX_STRING_LENGTH } from "./buffer-constants.js";
import { require_buffer } from "../vendor/buffer.js";
import { __toESM } from "../vendor/esbuild-runtime.js";

var import_buffer2 = __toESM(require_buffer(), 1);

var bufferPolyfillMutable = import_buffer2.Buffer;

// This is an implementation-detail flag from the browser Buffer polyfill.
// Node's Buffer does not expose it, and packages use its presence to reject
// non-native Buffer implementations.
delete bufferPolyfillMutable.TYPED_ARRAY_SUPPORT;

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

// Native Node streams normalize byte-mode Uint8Array chunks to FastBuffer.
// Some userland stream implementations retain the Uint8Array identity while
// current Undici calls the FastBuffer decoding primitive directly.
if (typeof Uint8Array.prototype.utf8Slice !== "function") {
  Object.defineProperty(Uint8Array.prototype, "utf8Slice", {
    configurable: true,
    writable: true,
    value(start, end) {
      return import_buffer2.Buffer.from(this.buffer, this.byteOffset, this.byteLength).toString(
        "utf8",
        start,
        end
      );
    }
  });
}

var bufferCtorMutable = import_buffer2.Buffer;

if (typeof bufferCtorMutable.allocUnsafe === "function" && !bufferCtorMutable.allocUnsafe._agentOsPatched) {
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
  bufferCtorMutable.allocUnsafe._agentOsPatched = true;
}

var Buffer3 = import_buffer2.Buffer;

function hasStandardsCompliantTypedArrayBase64() {
  try {
    if (typeof Uint8Array.fromBase64 !== "function" || typeof Uint8Array.prototype.toBase64 !== "function") {
      return false;
    }
    const encoded = new Uint8Array([251, 255]).toBase64({ alphabet: "base64url", omitPadding: true });
    const decoded = Uint8Array.fromBase64("-_8", { alphabet: "base64url" });
    return encoded === "-_8" && decoded.length === 2 && decoded[0] === 251 && decoded[1] === 255;
  } catch {
    return false;
  }
}

if (!hasStandardsCompliantTypedArrayBase64()) {
  Object.defineProperty(Uint8Array, "fromBase64", {
    configurable: true,
    writable: true,
    value(value, options = {}) {
      const alphabet = options?.alphabet ?? "base64";
      let encoded = String(value);
      if (alphabet === "base64url") {
        encoded = encoded.replace(/-/g, "+").replace(/_/g, "/");
      } else if (alphabet !== "base64") {
        throw new TypeError(`Unknown alphabet: ${alphabet}`);
      }
      return Uint8Array.from(Buffer3.from(encoded, "base64"));
    }
  });
  Object.defineProperty(Uint8Array.prototype, "toBase64", {
    configurable: true,
    writable: true,
    value(options = {}) {
      const alphabet = options?.alphabet ?? "base64";
      let encoded = Buffer3.from(this.buffer, this.byteOffset, this.byteLength).toString("base64");
      if (alphabet === "base64url") {
        encoded = encoded.replace(/\+/g, "-").replace(/\//g, "_");
      } else if (alphabet !== "base64") {
        throw new TypeError(`Unknown alphabet: ${alphabet}`);
      }
      return options?.omitPadding ? encoded.replace(/=+$/g, "") : encoded;
    }
  });
}
export { Buffer3, bufferCtorMutable, bufferPolyfillMutable, bufferProto, import_buffer2 };
