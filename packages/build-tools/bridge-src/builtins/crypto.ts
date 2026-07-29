import { builtinConstantsStdlibModule } from "./builtin-modules.js";
import { import_buffer2 } from "./buffer-runtime.js";

function createUnsupportedCryptoApiError(subject) {
  const error = new Error(`${subject} is not supported in sandbox`);
  error.code = "ERR_NOT_IMPLEMENTED";
  return error;
}

function throwUnsupportedCryptoApi(api) {
  throw createUnsupportedCryptoApiError(`crypto.${api}`);
}

var kCryptoKeyToken = /* @__PURE__ */ Symbol("agentOsCryptoKey");

var kCryptoToken = /* @__PURE__ */ Symbol("agentOsCrypto");

var kSubtleToken = /* @__PURE__ */ Symbol("agentOsSubtle");

var ERR_INVALID_THIS2 = "ERR_INVALID_THIS";

var ERR_ILLEGAL_CONSTRUCTOR = "ERR_ILLEGAL_CONSTRUCTOR";

function createNodeTypeError2(message, code) {
  const error = new TypeError(message);
  error.code = code;
  return error;
}

class SandboxDOMException extends Error {
  constructor(message = "", name = "Error") {
    super(message);
    this.name = String(name);
    this.code = 0;
  }
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

var MAX_CRYPTO_HASH_UPDATE_BYTES = 1024 * 1024;

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
      if (typeof _cryptoHashCreate !== "undefined" && typeof _cryptoHashUpdate !== "undefined" && typeof _cryptoHashFinal !== "undefined") {
        const sessionId = _cryptoHashCreate.applySync(void 0, [String(algorithm)]);
        try {
          for (let offset = 0; offset < input.length; offset += MAX_CRYPTO_HASH_UPDATE_BYTES) {
            _cryptoHashUpdate.applySync(void 0, [
              sessionId,
              input.subarray(offset, offset + MAX_CRYPTO_HASH_UPDATE_BYTES)
            ]);
          }
          const resultBase64 = _cryptoHashFinal.applySync(void 0, [sessionId]);
          const result = import_buffer2.Buffer.from(String(resultBase64 || ""), "base64");
          return encoding ? result.toString(encoding) : result;
        } catch (error) {
          if (typeof _cryptoHashDestroy !== "undefined") {
            try {
              _cryptoHashDestroy.applySync(void 0, [sessionId]);
            } catch (cleanupError) {
              if (error && typeof error === "object" && !("cause" in error)) {
                error.cause = cleanupError;
              }
            }
          }
          throw error;
        }
      }
      const resultBase64 = _cryptoHashDigest.applySync(void 0, [
        String(algorithm),
        input.toString("base64")
      ]);
      const result = import_buffer2.Buffer.from(String(resultBase64 || ""), "base64");
      return encoding ? result.toString(encoding) : result;
    }
  };
}

function toSymmetricKeyBuffer(key) {
  if (isBuiltinKeyObject(key)) {
    if (key.type !== "secret") {
      throw new TypeError("Symmetric crypto operations require a secret KeyObject");
    }
    return import_buffer2.Buffer.from(String(key._serialized.raw || ""), "base64");
  }
  if (key && typeof key === "object" && key[Symbol.toStringTag] === "CryptoKey" && typeof key._raw === "string") {
    return import_buffer2.Buffer.from(String(key._raw || ""), "base64");
  }
  return typeof key === "string" ? import_buffer2.Buffer.from(key) : toCryptoBuffer(key);
}

function createBuiltinHmac(algorithm, key) {
  const chunks = [];
  const keyBuffer = toSymmetricKeyBuffer(key);
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

var kBuiltinCryptoKeyObjectToken = /* @__PURE__ */ Symbol("agentOsBuiltinKeyObject");

function isBufferLikeValue(value) {
  return import_buffer2.Buffer.isBuffer(value) || value instanceof ArrayBuffer || ArrayBuffer.isView(value);
}

function toCryptoBuffer(value, encoding = void 0) {
  if (import_buffer2.Buffer.isBuffer(value)) {
    return import_buffer2.Buffer.from(value);
  }
  if (typeof value === "string") {
    return import_buffer2.Buffer.from(value, encoding || "utf8");
  }
  if (value instanceof ArrayBuffer) {
    return import_buffer2.Buffer.from(new Uint8Array(value));
  }
  if (ArrayBuffer.isView(value)) {
    return import_buffer2.Buffer.from(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
  }
  return import_buffer2.Buffer.from(value ?? []);
}

function encodeCryptoResult(buffer, encoding = void 0) {
  return encoding ? buffer.toString(encoding) : buffer;
}

function isBuiltinKeyObject(value) {
  return Boolean(value && typeof value === "object" && (value[kBuiltinCryptoKeyObjectToken] === true || value[Symbol.toStringTag] === "KeyObject" && "_serialized" in value));
}

function serializeBridgeValue(value) {
  if (isBuiltinKeyObject(value)) {
    return value._serialized;
  }
  if (value && typeof value === "object" && value[Symbol.toStringTag] === "CryptoKey" && "_keyData" in value) {
    return value._keyData;
  }
  if (typeof value === "bigint") {
    return {
      __type: "bigint",
      value: value.toString()
    };
  }
  if (isBufferLikeValue(value)) {
    return {
      __type: "buffer",
      value: toCryptoBuffer(value).toString("base64")
    };
  }
  if (Array.isArray(value)) {
    return value.map((entry) => serializeBridgeValue(entry));
  }
  if (value && typeof value === "object") {
    const normalized = {};
    for (const [key, entry] of Object.entries(value)) {
      if (entry !== void 0) {
        normalized[key] = serializeBridgeValue(entry);
      }
    }
    return normalized;
  }
  return value;
}

function deserializeBridgeValue(value) {
  if (Array.isArray(value)) {
    return value.map((entry) => deserializeBridgeValue(entry));
  }
  if (!value || typeof value !== "object") {
    return value;
  }
  if (value.__type === "buffer") {
    return import_buffer2.Buffer.from(String(value.value || ""), "base64");
  }
  if (value.__type === "bigint") {
    return BigInt(String(value.value || "0"));
  }
  if (value.__type === "keyObject") {
    return createBuiltinKeyObject(value.value);
  }
  const normalized = {};
  for (const [key, entry] of Object.entries(value)) {
    normalized[key] = deserializeBridgeValue(entry);
  }
  return normalized;
}

function normalizeDirectCryptoKeyInput(key) {
  if (isBuiltinKeyObject(key)) {
    return key._serialized;
  }
  if (key && typeof key === "object" && key[Symbol.toStringTag] === "CryptoKey" && "_keyData" in key) {
    return key._keyData;
  }
  if (key && typeof key === "object" && !Array.isArray(key) && "key" in key) {
    const { key: keySource, ...rest } = key;
    const normalizedSource = normalizeDirectCryptoKeyInput(keySource);
    if (normalizedSource && typeof normalizedSource === "object" && !Array.isArray(normalizedSource) && ("type" in normalizedSource && ("pem" in normalizedSource || "raw" in normalizedSource))) {
      return {
        ...normalizedSource,
        ...Object.fromEntries(Object.entries(rest).map(([name, value]) => [name, serializeBridgeValue(value)]))
      };
    }
    return {
      ...Object.fromEntries(Object.entries(rest).map(([name, value]) => [name, serializeBridgeValue(value)])),
      key: serializeBridgeValue(keySource)
    };
  }
  return serializeBridgeValue(key);
}

function serializeCryptoKeyInput(key) {
  return JSON.stringify(normalizeDirectCryptoKeyInput(key));
}

function serializeOptionalCryptoOptions(options) {
  return JSON.stringify({
    hasOptions: options !== void 0,
    options: options === void 0 ? null : serializeBridgeValue(options)
  });
}

function normalizeCryptoAlgorithmName(algorithm) {
  if (algorithm == null) {
    return null;
  }
  if (typeof algorithm === "string") {
    return algorithm;
  }
  if (typeof algorithm === "object" && algorithm && typeof algorithm.name === "string") {
    return algorithm.name;
  }
  return String(algorithm);
}

function callCryptoSync(bridge, api, args) {
  if (typeof bridge === "undefined") {
    throwUnsupportedCryptoApi(api);
  }
  return bridge.applySync(void 0, args);
}

function decodeGeneratedCryptoValue(value) {
  if (value && typeof value === "object" && value.kind === "buffer") {
    return import_buffer2.Buffer.from(String(value.value || ""), "base64");
  }
  if (value && typeof value === "object" && value.kind === "string") {
    return String(value.value || "");
  }
  return createBuiltinKeyObject(value);
}

class BuiltinKeyObject {
  type;
  asymmetricKeyType;
  asymmetricKeyDetails;
  symmetricKeySize;
  _serialized;
  [kBuiltinCryptoKeyObjectToken];
  constructor(serialized, token) {
    if (token !== kBuiltinCryptoKeyObjectToken || !serialized || typeof serialized !== "object") {
      throw createNodeTypeError2("Illegal constructor", ERR_ILLEGAL_CONSTRUCTOR);
    }
    this.type = serialized.type;
    this.asymmetricKeyType = serialized.asymmetricKeyType;
    this.asymmetricKeyDetails = serialized.asymmetricKeyDetails;
    this.symmetricKeySize = serialized.raw ? import_buffer2.Buffer.from(String(serialized.raw), "base64").length : void 0;
    this._serialized = serialized;
    this[kBuiltinCryptoKeyObjectToken] = true;
  }
  export(options = void 0) {
    if (this.type === "secret") {
      if (options && options.format === "jwk" && this._serialized.jwk) {
        return { ...this._serialized.jwk };
      }
      return import_buffer2.Buffer.from(String(this._serialized.raw || ""), "base64");
    }
    if (options == null || typeof options !== "object") {
      const error = new TypeError('The "options" argument must be of type object. Received undefined');
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    if (options.format === "jwk" && this._serialized.jwk) {
      return { ...this._serialized.jwk };
    }
    if (options.format && options.format !== "pem") {
      throw createUnsupportedCryptoApiError(`crypto.KeyObject.export(${options.format})`);
    }
    return String(this._serialized.pem || "");
  }
  equals(other) {
    return isBuiltinKeyObject(other) && JSON.stringify(this._serialized) === JSON.stringify(other._serialized);
  }
}

Object.defineProperty(BuiltinKeyObject.prototype, Symbol.toStringTag, {
  value: "KeyObject",
  configurable: true
});

Object.defineProperty(BuiltinKeyObject, Symbol.hasInstance, {
  value(candidate) {
    return isBuiltinKeyObject(candidate);
  },
  configurable: true
});

function createBuiltinKeyObject(serialized) {
  if (isBuiltinKeyObject(serialized)) {
    return serialized;
  }
  return new BuiltinKeyObject(serialized, kBuiltinCryptoKeyObjectToken);
}

function normalizeCipherOptions(options) {
  if (!options || typeof options !== "object") {
    return {};
  }
  const normalized = {};
  if ("aad" in options && options.aad != null) {
    normalized.aad = toCryptoBuffer(options.aad).toString("base64");
  }
  if ("authTag" in options && options.authTag != null) {
    normalized.authTag = toCryptoBuffer(options.authTag).toString("base64");
  }
  if ("authTagLength" in options && options.authTagLength != null) {
    normalized.authTagLength = Number(options.authTagLength);
  }
  if ("autoPadding" in options) {
    normalized.autoPadding = Boolean(options.autoPadding);
  }
  return normalized;
}

class BuiltinCipherTransform {
  _mode;
  _algorithm;
  _key;
  _iv;
  _options;
  _sessionId;
  _authTag;
  constructor(mode, algorithm, key, iv, options = void 0) {
    this._mode = mode;
    this._algorithm = String(algorithm);
    this._key = toSymmetricKeyBuffer(key);
    this._iv = iv == null ? null : toCryptoBuffer(iv);
    this._options = normalizeCipherOptions(options);
    this._sessionId = null;
    this._authTag = null;
  }
  _ensureSession() {
    if (this._sessionId != null) {
      return this._sessionId;
    }
    this._sessionId = Number(callCryptoSync(_cryptoCipherivCreate, this._mode === "cipher" ? "createCipheriv" : "createDecipheriv", [
      this._mode,
      this._algorithm,
      this._key.toString("base64"),
      this._iv ? this._iv.toString("base64") : null,
      Object.keys(this._options).length > 0 ? JSON.stringify(this._options) : null
    ]));
    return this._sessionId;
  }
  _assertMutable(methodName) {
    if (this._sessionId != null) {
      throw createUnsupportedCryptoApiError(`crypto.${methodName} after update()`);
    }
  }
  update(data, inputEncoding = void 0, outputEncoding = void 0) {
    const chunk = toCryptoBuffer(data, inputEncoding);
    const resultBase64 = callCryptoSync(_cryptoCipherivUpdate, this._mode === "cipher" ? "createCipheriv" : "createDecipheriv", [
      this._ensureSession(),
      chunk.toString("base64")
    ]);
    return encodeCryptoResult(import_buffer2.Buffer.from(String(resultBase64 || ""), "base64"), outputEncoding);
  }
  final(outputEncoding = void 0) {
    const response = JSON.parse(String(callCryptoSync(_cryptoCipherivFinal, this._mode === "cipher" ? "createCipheriv" : "createDecipheriv", [this._ensureSession()]) || "{}"));
    if (response.authTag) {
      this._authTag = import_buffer2.Buffer.from(String(response.authTag), "base64");
    }
    return encodeCryptoResult(import_buffer2.Buffer.from(String(response.data || ""), "base64"), outputEncoding);
  }
  setAAD(buffer, options = void 0) {
    this._assertMutable(this._mode === "cipher" ? "createCipheriv" : "createDecipheriv");
    this._options.aad = toCryptoBuffer(buffer).toString("base64");
    if (options && typeof options === "object" && options.authTagLength != null) {
      this._options.authTagLength = Number(options.authTagLength);
    }
    return this;
  }
  setAuthTag(buffer) {
    this._assertMutable("createDecipheriv");
    this._authTag = toCryptoBuffer(buffer);
    this._options.authTag = this._authTag.toString("base64");
    return this;
  }
  getAuthTag() {
    if (!this._authTag) {
      throw new Error("Invalid state for operation getAuthTag");
    }
    return import_buffer2.Buffer.from(this._authTag);
  }
  setAutoPadding(autoPadding = true) {
    this._assertMutable(this._mode === "cipher" ? "createCipheriv" : "createDecipheriv");
    this._options.autoPadding = Boolean(autoPadding);
    return this;
  }
}

class BuiltinSignContext {
  _algorithm;
  _chunks;
  constructor(algorithm) {
    this._algorithm = algorithm;
    this._chunks = [];
  }
  update(data, encoding = void 0) {
    this._chunks.push(toCryptoBuffer(data, encoding));
    return this;
  }
  write(data, encoding = void 0) {
    this.update(data, encoding);
    return true;
  }
  end(data = void 0, encoding = void 0) {
    if (data !== void 0) {
      this.update(data, encoding);
    }
    return this;
  }
  _inputBuffer() {
    if (this._chunks.length === 0) {
      return import_buffer2.Buffer.alloc(0);
    }
    return this._chunks.length === 1 ? this._chunks[0] : import_buffer2.Buffer.concat(this._chunks);
  }
  sign(key, outputEncoding = void 0) {
    const resultBase64 = callCryptoSync(_cryptoSign, "sign", [
      normalizeCryptoAlgorithmName(this._algorithm),
      this._inputBuffer().toString("base64"),
      serializeCryptoKeyInput(key)
    ]);
    return encodeCryptoResult(import_buffer2.Buffer.from(String(resultBase64 || ""), "base64"), outputEncoding);
  }
}

class BuiltinVerifyContext extends BuiltinSignContext {
  verify(key, signature, signatureEncoding = void 0) {
    const signatureBuffer = toCryptoBuffer(signature, signatureEncoding);
    return Boolean(callCryptoSync(_cryptoVerify, "verify", [
      normalizeCryptoAlgorithmName(this._algorithm),
      this._inputBuffer().toString("base64"),
      serializeCryptoKeyInput(key),
      signatureBuffer.toString("base64")
    ]));
  }
}

function normalizeDiffieHellmanArgs(sizeOrKey, keyEncoding = void 0, generator = void 0, generatorEncoding = void 0) {
  const args = [];
  if (typeof sizeOrKey === "string") {
    args.push(toCryptoBuffer(sizeOrKey, typeof keyEncoding === "string" ? keyEncoding : void 0));
    if (generator !== void 0) {
      args.push(typeof generator === "string" ? toCryptoBuffer(generator, typeof generatorEncoding === "string" ? generatorEncoding : void 0) : generator);
    } else if (typeof keyEncoding === "number" || isBufferLikeValue(keyEncoding)) {
      args.push(keyEncoding);
    }
    return args;
  }
  args.push(sizeOrKey);
  if (generator !== void 0) {
    args.push(generator);
  } else if (typeof keyEncoding === "number" || isBufferLikeValue(keyEncoding)) {
    args.push(keyEncoding);
  }
  return args;
}

const diffieHellmanSessionFinalizer = typeof FinalizationRegistry === "function" ? new FinalizationRegistry((sessionId) => {
  try {
    callCryptoSync(_cryptoDiffieHellmanSessionDestroy, "createDiffieHellman", [sessionId]);
  } catch {
  }
}) : null;

class BuiltinDiffieHellmanSession {
  _sessionId;
  constructor(request) {
    this._sessionId = Number(callCryptoSync(_cryptoDiffieHellmanSessionCreate, "createDiffieHellman", [JSON.stringify({
      type: request.type,
      name: request.name,
      args: (request.args || []).map((entry) => serializeBridgeValue(entry))
    })]));
    diffieHellmanSessionFinalizer?.register(this, this._sessionId, this);
  }
  _destroySession() {
    if (this._sessionId == null) {
      return;
    }
    const sessionId = this._sessionId;
    this._sessionId = null;
    diffieHellmanSessionFinalizer?.unregister(this);
    callCryptoSync(_cryptoDiffieHellmanSessionDestroy, "createDiffieHellman", [sessionId]);
  }
  dispose() {
    this._destroySession();
  }
  [Symbol.dispose || Symbol.for("Symbol.dispose")]() {
    this._destroySession();
  }
  _call(method, args = []) {
    if (this._sessionId == null) {
      throw new Error("Diffie-Hellman session has been destroyed");
    }
    const response = JSON.parse(String(callCryptoSync(_cryptoDiffieHellmanSessionCall, "createDiffieHellman", [
      this._sessionId,
      JSON.stringify({
        method,
        args: args.map((entry) => serializeBridgeValue(entry))
      })
    ]) || "{}"));
    return response.hasResult ? deserializeBridgeValue(response.result) : void 0;
  }
  get verifyError() {
    const value = this._call("verifyError");
    return value == null ? 0 : Number(value);
  }
  generateKeys(encoding = void 0) {
    return encodeCryptoResult(toCryptoBuffer(this._call("generateKeys")), encoding);
  }
  computeSecret(otherPublicKey, inputEncoding = void 0, outputEncoding = void 0) {
    const result = this._call("computeSecret", [toCryptoBuffer(otherPublicKey, inputEncoding)]);
    return encodeCryptoResult(toCryptoBuffer(result), outputEncoding);
  }
  getPrime(encoding = void 0) {
    return encodeCryptoResult(toCryptoBuffer(this._call("getPrime")), encoding);
  }
  getGenerator(encoding = void 0) {
    return encodeCryptoResult(toCryptoBuffer(this._call("getGenerator")), encoding);
  }
  getPublicKey(encoding = void 0) {
    return encodeCryptoResult(toCryptoBuffer(this._call("getPublicKey")), encoding);
  }
  getPrivateKey(encoding = void 0) {
    return encodeCryptoResult(toCryptoBuffer(this._call("getPrivateKey")), encoding);
  }
  setPrivateKey(privateKey, encoding = void 0) {
    this._call("setPrivateKey", [toCryptoBuffer(privateKey, encoding)]);
  }
  setPublicKey(publicKey, encoding = void 0) {
    this._call("setPublicKey", [toCryptoBuffer(publicKey, encoding)]);
  }
}

var builtinCryptoModule = {
  KeyObject: BuiltinKeyObject,
  DiffieHellman: BuiltinDiffieHellmanSession,
  ECDH: BuiltinDiffieHellmanSession,
  randomFillSync(buffer, offset = 0, size = void 0) {
    const target = buffer instanceof ArrayBuffer ? new Uint8Array(buffer) : buffer;
    if (!ArrayBuffer.isView(target)) {
      throw new TypeError(
        'The "buffer" argument must be an instance of ArrayBuffer, Buffer, TypedArray, or DataView'
      );
    }
    const start = Number(offset) || 0;
    if (!Number.isInteger(start) || start < 0 || start > target.byteLength) {
      throw new RangeError('The value of "offset" is out of range');
    }
    const length = size === void 0 ? target.byteLength - start : Number(size);
    if (!Number.isInteger(length) || length < 0 || start + length > target.byteLength) {
      throw new RangeError('The value of "size" is out of range');
    }
    const view = new Uint8Array(target.buffer, target.byteOffset + start, length);
    cryptoPolyfill.getRandomValues(view);
    return buffer;
  },
  randomFill(buffer, offset, size, callback) {
    let start = offset;
    let length = size;
    let done = callback;
    if (typeof start === "function") {
      done = start;
      start = 0;
      length = void 0;
    } else if (typeof length === "function") {
      done = length;
      length = void 0;
    }
    if (typeof done !== "function") {
      throw new TypeError('The "callback" argument must be of type function');
    }
    try {
      const result = builtinCryptoModule.randomFillSync(buffer, start, length);
      queueMicrotask(() => done(null, result));
    } catch (error) {
      queueMicrotask(() => done(error));
    }
  },
  createHash(algorithm) {
    return createBuiltinHash(algorithm);
  },
  createHmac(algorithm, key) {
    return createBuiltinHmac(algorithm, key);
  },
  createCipheriv(algorithm, key, iv, options = void 0) {
    return new BuiltinCipherTransform("cipher", algorithm, key, iv, options);
  },
  createDecipheriv(algorithm, key, iv, options = void 0) {
    return new BuiltinCipherTransform("decipher", algorithm, key, iv, options);
  },
  createSign(algorithm) {
    return new BuiltinSignContext(algorithm);
  },
  createVerify(algorithm) {
    return new BuiltinVerifyContext(algorithm);
  },
  sign(algorithm, data, key) {
    const signer = new BuiltinSignContext(algorithm);
    signer.update(data);
    return signer.sign(key);
  },
  verify(algorithm, data, key, signature) {
    const verifier = new BuiltinVerifyContext(algorithm);
    verifier.update(data);
    return verifier.verify(key, signature);
  },
  createPrivateKey(key) {
    const payload = callCryptoSync(_cryptoCreateKeyObject, "createPrivateKey", [
      "createPrivateKey",
      serializeCryptoKeyInput(key)
    ]);
    return createBuiltinKeyObject(JSON.parse(String(payload || "{}")));
  },
  createPublicKey(key) {
    const payload = callCryptoSync(_cryptoCreateKeyObject, "createPublicKey", [
      "createPublicKey",
      serializeCryptoKeyInput(key)
    ]);
    return createBuiltinKeyObject(JSON.parse(String(payload || "{}")));
  },
  createSecretKey(key) {
    return createBuiltinKeyObject({
      type: "secret",
      raw: toCryptoBuffer(key).toString("base64")
    });
  },
  publicEncrypt(key, buffer) {
    const payload = callCryptoSync(_cryptoAsymmetricOp, "publicEncrypt", [
      "publicEncrypt",
      serializeCryptoKeyInput(key),
      toCryptoBuffer(buffer).toString("base64")
    ]);
    return import_buffer2.Buffer.from(String(payload || ""), "base64");
  },
  publicDecrypt(key, buffer) {
    const payload = callCryptoSync(_cryptoAsymmetricOp, "publicDecrypt", [
      "publicDecrypt",
      serializeCryptoKeyInput(key),
      toCryptoBuffer(buffer).toString("base64")
    ]);
    return import_buffer2.Buffer.from(String(payload || ""), "base64");
  },
  privateEncrypt(key, buffer) {
    const payload = callCryptoSync(_cryptoAsymmetricOp, "privateEncrypt", [
      "privateEncrypt",
      serializeCryptoKeyInput(key),
      toCryptoBuffer(buffer).toString("base64")
    ]);
    return import_buffer2.Buffer.from(String(payload || ""), "base64");
  },
  privateDecrypt(key, buffer) {
    const payload = callCryptoSync(_cryptoAsymmetricOp, "privateDecrypt", [
      "privateDecrypt",
      serializeCryptoKeyInput(key),
      toCryptoBuffer(buffer).toString("base64")
    ]);
    return import_buffer2.Buffer.from(String(payload || ""), "base64");
  },
  pbkdf2Sync(password, salt, iterations, keylen, digest) {
    const payload = callCryptoSync(_cryptoPbkdf2, "pbkdf2Sync", [
      toCryptoBuffer(password).toString("base64"),
      toCryptoBuffer(salt).toString("base64"),
      Number(iterations),
      Number(keylen),
      String(digest)
    ]);
    return import_buffer2.Buffer.from(String(payload || ""), "base64");
  },
  pbkdf2(password, salt, iterations, keylen, digest, callback) {
    let algorithm = digest;
    let done = callback;
    if (typeof algorithm === "function") {
      done = algorithm;
      algorithm = "sha1";
    }
    if (typeof done !== "function") {
      throw new TypeError('The "callback" argument must be of type function');
    }
    queueMicrotask(() => {
      try {
        done(null, builtinCryptoModule.pbkdf2Sync(password, salt, iterations, keylen, algorithm));
      } catch (error) {
        done(error);
      }
    });
  },
  scryptSync(password, salt, keylen, options = void 0) {
    const payload = callCryptoSync(_cryptoScrypt, "scryptSync", [
      toCryptoBuffer(password).toString("base64"),
      toCryptoBuffer(salt).toString("base64"),
      Number(keylen),
      JSON.stringify(serializeBridgeValue(options || {}))
    ]);
    return import_buffer2.Buffer.from(String(payload || ""), "base64");
  },
  scrypt(password, salt, keylen, options, callback) {
    let normalizedOptions = options;
    let done = callback;
    if (typeof normalizedOptions === "function") {
      done = normalizedOptions;
      normalizedOptions = void 0;
    }
    if (typeof done !== "function") {
      throw new TypeError('The "callback" argument must be of type function');
    }
    queueMicrotask(() => {
      try {
        done(null, builtinCryptoModule.scryptSync(password, salt, keylen, normalizedOptions));
      } catch (error) {
        done(error);
      }
    });
  },
  generateKeyPairSync(type, options = void 0) {
    const payload = JSON.parse(String(callCryptoSync(_cryptoGenerateKeyPairSync, "generateKeyPairSync", [
      String(type),
      serializeOptionalCryptoOptions(options)
    ]) || "{}"));
    return {
      publicKey: decodeGeneratedCryptoValue(payload.publicKey),
      privateKey: decodeGeneratedCryptoValue(payload.privateKey)
    };
  },
  generateKeyPair(type, options, callback) {
    let normalizedOptions = options;
    let done = callback;
    if (typeof normalizedOptions === "function") {
      done = normalizedOptions;
      normalizedOptions = void 0;
    }
    if (typeof done !== "function") {
      throw new TypeError('The "callback" argument must be of type function');
    }
    queueMicrotask(() => {
      try {
        const result = builtinCryptoModule.generateKeyPairSync(type, normalizedOptions);
        done(null, result.publicKey, result.privateKey);
      } catch (error) {
        done(error);
      }
    });
  },
  generateKeySync(type, options = void 0) {
    const payload = JSON.parse(String(callCryptoSync(_cryptoGenerateKeySync, "generateKeySync", [
      String(type),
      serializeOptionalCryptoOptions(options)
    ]) || "{}"));
    return createBuiltinKeyObject(payload);
  },
  generatePrimeSync(size, options = void 0) {
    const payload = JSON.parse(String(callCryptoSync(_cryptoGeneratePrimeSync, "generatePrimeSync", [
      Number(size),
      serializeOptionalCryptoOptions(options)
    ]) || "null"));
    return deserializeBridgeValue(payload);
  },
  generatePrime(size, options, callback) {
    let normalizedOptions = options;
    let done = callback;
    if (typeof normalizedOptions === "function") {
      done = normalizedOptions;
      normalizedOptions = void 0;
    }
    if (typeof done !== "function") {
      throw new TypeError('The "callback" argument must be of type function');
    }
    queueMicrotask(() => {
      try {
        done(null, builtinCryptoModule.generatePrimeSync(size, normalizedOptions));
      } catch (error) {
        done(error);
      }
    });
  },
  diffieHellman(options) {
    const payload = JSON.parse(String(callCryptoSync(_cryptoDiffieHellman, "diffieHellman", [
      JSON.stringify(serializeBridgeValue(options))
    ]) || "null"));
    return toCryptoBuffer(deserializeBridgeValue(payload));
  },
  getDiffieHellman(name) {
    return new BuiltinDiffieHellmanSession({ type: "group", name: String(name) });
  },
  createDiffieHellman(sizeOrKey, keyEncoding = void 0, generator = void 0, generatorEncoding = void 0) {
    return new BuiltinDiffieHellmanSession({
      type: "dh",
      args: normalizeDiffieHellmanArgs(sizeOrKey, keyEncoding, generator, generatorEncoding)
    });
  },
  createECDH(curve) {
    return new BuiltinDiffieHellmanSession({ type: "ecdh", name: String(curve) });
  },
  getFips() {
    return 0;
  },
  getHashes() {
    return ["md5", "sha1", "sha224", "sha256", "sha384", "sha512"];
  },
  getCiphers() {
    return [
      "aes-128-cbc",
      "aes-128-ctr",
      "aes-128-gcm",
      "aes-192-cbc",
      "aes-192-ctr",
      "aes-192-gcm",
      "aes-256-cbc",
      "aes-256-ctr",
      "aes-256-gcm",
      "aes128",
      "aes192",
      "aes256"
    ];
  },
  getCurves() {
    return ["prime256v1", "secp256k1", "secp384r1", "secp521r1"];
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
  get constants() {
    return builtinConstantsStdlibModule;
  },
  subtle: subtleCrypto,
  webcrypto: cryptoPolyfill
};
export { BuiltinCipherTransform, BuiltinDiffieHellmanSession, BuiltinKeyObject, BuiltinSignContext, BuiltinVerifyContext, ERR_ILLEGAL_CONSTRUCTOR, ERR_INVALID_THIS2, SandboxCrypto, SandboxCryptoKey, SandboxDOMException, SandboxSubtleCrypto, assertCryptoReceiver, assertSubtleReceiver, builtinCryptoModule, callCryptoSync, createBuiltinHash, createBuiltinHmac, createBuiltinKeyObject, createCryptoKey, createDomLikeError, createNodeTypeError2, createUnsupportedCryptoApiError, cryptoPolyfill, cryptoPolyfillInstance, decodeGeneratedCryptoValue, deserializeBridgeValue, diffieHellmanSessionFinalizer, encodeCryptoResult, isBufferLikeValue, isBuiltinKeyObject, isIntegerTypedArray, kBuiltinCryptoKeyObjectToken, kCryptoKeyToken, kCryptoToken, kSubtleToken, normalizeAlgorithm, normalizeBridgeAlgorithm, normalizeCipherOptions, normalizeCryptoAlgorithmName, normalizeDiffieHellmanArgs, normalizeDirectCryptoKeyInput, serializeBridgeValue, serializeCryptoKeyInput, serializeOptionalCryptoOptions, subtleCall, subtleCrypto, throwUnsupportedCryptoApi, toArrayBuffer, toBase64, toCryptoBuffer, toSymmetricKeyBuffer };
