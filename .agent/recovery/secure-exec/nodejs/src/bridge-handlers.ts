// Build a BridgeHandlers map for V8 runtime.
//
// Each handler is a plain function that performs the host-side operation.
// Handler names match HOST_BRIDGE_GLOBAL_KEYS from the bridge contract.

import * as net from "node:net";
import * as http from "node:http";
import * as https from "node:https";
import * as http2 from "node:http2";
import * as tls from "node:tls";
import * as hostUtil from "node:util";
import * as zlib from "node:zlib";
import { Duplex, PassThrough } from "node:stream";
import { readFileSync, realpathSync, existsSync } from "node:fs";
import { dirname as pathDirname, join as pathJoin, resolve as pathResolve } from "node:path";
import { createRequire } from "node:module";
import v8Mod from "node:v8";

// Bun's node:v8 module doesn't produce real V8 serialization format
const _isBun = typeof (globalThis as Record<string, unknown>).Bun !== "undefined";
let _cbor: typeof import("cbor-x") | null = null;
function _getCbor(): typeof import("cbor-x") {
	if (!_cbor) {
		// eslint-disable-next-line @typescript-eslint/no-require-imports
		_cbor = require("cbor-x") as typeof import("cbor-x");
	}
	return _cbor;
}
function ipcSerialize(value: unknown): Buffer {
	if (_isBun) return Buffer.from(_getCbor().encode(value));
	return Buffer.from(v8Mod.serialize(value));
}
import {
	randomFillSync,
	randomUUID,
	createHash,
	createHmac,
	pbkdf2Sync,
	scryptSync,
	hkdfSync,
	createCipheriv,
	createDecipheriv,
	sign,
	verify,
	generateKeyPairSync,
	createPrivateKey,
	createPublicKey,
	createSecretKey,
	createDiffieHellman,
	getDiffieHellman,
	createECDH,
	diffieHellman,
	generateKeySync,
	generatePrimeSync,
	publicEncrypt,
	privateDecrypt,
	privateEncrypt,
	publicDecrypt,
	timingSafeEqual,
	constants as cryptoConstants,
	KeyObject,
	type Cipher,
	type Decipher,
} from "node:crypto";
import {
	HOST_BRIDGE_GLOBAL_KEYS,
} from "./bridge-contract.js";
import {
	AF_INET,
	AF_INET6,
	AF_UNIX,
	SOCK_DGRAM,
	SOCK_STREAM,
	mkdir,
	FDTableManager,
	O_RDONLY,
	O_WRONLY,
	O_RDWR,
	O_CREAT,
	O_TRUNC,
	O_APPEND,
	FILETYPE_REGULAR_FILE,
} from "@secure-exec/core";
import { normalizeBuiltinSpecifier } from "./builtin-modules.js";
import { resolveModule, loadFile } from "./package-bundler.js";
import { bundlePolyfill, hasPolyfill } from "./polyfills.js";
import {
	createBuiltinESMWrapper,
	getBuiltinBindingExpression,
	getStaticBuiltinWrapperSource,
	getEmptyBuiltinESMWrapper,
} from "./esm-compiler.js";
import {
	transformSourceForImport,
	transformSourceForImportSync,
	transformSourceForRequire,
	transformSourceForRequireSync,
} from "./module-source.js";
import {
	checkBridgeBudget,
	assertPayloadByteLength,
	assertTextPayloadSize,
	getBase64EncodedByteLength,
	getHostBuiltinNamedExports,
	parseJsonWithLimit,
	polyfillCodeCache,
	RESOURCE_BUDGET_ERROR_CODE,
} from "./isolate-bootstrap.js";
import type {
	CommandExecutor,
	NetworkAdapter,
	SpawnedProcess,
} from "@secure-exec/core";
import type { VirtualFileSystem } from "@secure-exec/core";
import type { ResolutionCache } from "./package-bundler.js";
import type {
	StdioEvent,
	StdioHook,
	ProcessConfig,
} from "@secure-exec/core/internal/shared/api-types";
import type { BudgetState } from "./isolate-bootstrap.js";

const SOL_SOCKET = 1;
const IPPROTO_TCP = 6;
const SO_KEEPALIVE = 9;
const SO_RCVBUF = 8;
const SO_SNDBUF = 7;
const TCP_NODELAY = 1;

/** A bridge handler function invoked when sandbox code calls a bridge global. */
export type BridgeHandler = (...args: unknown[]) => unknown | Promise<unknown>;

/** Map of bridge global names to their handler functions. */
export type BridgeHandlers = Record<string, BridgeHandler>;

/** Result of building crypto bridge handlers — includes dispose for session cleanup. */
export interface CryptoBridgeResult {
	handlers: BridgeHandlers;
	dispose: () => void;
}

type SerializedKeyValue =
	| {
		kind: "string";
		value: string;
	}
	| {
		kind: "buffer";
		value: string;
	}
	| {
		kind: "keyObject";
		value: SerializedSandboxKeyObject;
	}
	| {
		kind: "object";
		value: Record<string, unknown>;
	};

interface SerializedSandboxKeyObject {
	type: "public" | "private" | "secret";
	pem?: string;
	raw?: string;
	asymmetricKeyType?: string;
	asymmetricKeyDetails?: Record<string, unknown>;
	jwk?: Record<string, unknown>;
}

type SerializedBridgeValue =
	| null
	| boolean
	| number
	| string
	| {
			__type: "buffer";
			value: string;
	  }
	| {
			__type: "bigint";
			value: string;
	  }
	| {
			__type: "keyObject";
			value: SerializedSandboxKeyObject;
	  }
	| SerializedBridgeValue[]
	| {
			[key: string]: SerializedBridgeValue;
	  };

/** Stateful cipher/decipher session stored between bridge calls. */
interface CipherSession {
	cipher: Cipher | Decipher;
	algorithm: string;
}

interface SerializedDispatchError {
	message: string;
	name?: string;
	code?: string;
	stack?: string;
}

type DiffieHellmanSession =
	| ReturnType<typeof createDiffieHellman>
	| ReturnType<typeof getDiffieHellman>
	| ReturnType<typeof createECDH>;

function serializeKeyDetails(details: unknown): Record<string, unknown> | undefined {
	if (!details || typeof details !== "object") {
		return undefined;
	}

	return Object.fromEntries(
		Object.entries(details).map(([key, value]) => [
			key,
			typeof value === "bigint"
				? { __type: "bigint", value: value.toString() }
				: value,
		]),
	);
}

function serializeKeyValue(value: unknown): SerializedKeyValue {
	if (Buffer.isBuffer(value)) {
		return {
			kind: "buffer",
			value: value.toString("base64"),
		};
	}

	if (typeof value === "string") {
		return {
			kind: "string",
			value,
		};
	}

	if (
		value &&
		typeof value === "object" &&
		"type" in value &&
		((value as { type?: unknown }).type === "public" ||
			(value as { type?: unknown }).type === "private") &&
		typeof (value as { export?: unknown }).export === "function"
	) {
		return {
			kind: "keyObject",
			value: serializeSandboxKeyObject(value as any),
		};
	}

	return {
		kind: "object",
		value: value as Record<string, unknown>,
	};
}

function exportAsPem(keyObject: ReturnType<typeof createPrivateKey> | ReturnType<typeof createPublicKey>): string {
	return keyObject.type === "private"
		? (keyObject.export({ type: "pkcs8", format: "pem" }) as string)
		: (keyObject.export({ type: "spki", format: "pem" }) as string);
}

function serializeSandboxKeyObject(
	keyObject: ReturnType<typeof createPrivateKey> | ReturnType<typeof createPublicKey>,
): SerializedSandboxKeyObject {
	let jwk: Record<string, unknown> | undefined;
	try {
		jwk = keyObject.export({ format: "jwk" }) as Record<string, unknown>;
	} catch {
		jwk = undefined;
	}

	return {
		type: keyObject.type,
		pem: exportAsPem(keyObject),
		asymmetricKeyType: keyObject.asymmetricKeyType ?? undefined,
		asymmetricKeyDetails: serializeKeyDetails(keyObject.asymmetricKeyDetails),
		jwk,
	};
}

function serializeAnyKeyObject(keyObject: any): SerializedSandboxKeyObject {
	if (keyObject.type === "secret") {
		return {
			type: "secret",
			raw: Buffer.from(keyObject.export()).toString("base64"),
		};
	}

	return serializeSandboxKeyObject(keyObject);
}

function serializeBridgeValue(value: unknown): SerializedBridgeValue {
	if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
		return value;
	}

	if (typeof value === "bigint") {
		return {
			__type: "bigint",
			value: value.toString(),
		};
	}

	if (Buffer.isBuffer(value)) {
		return {
			__type: "buffer",
			value: value.toString("base64"),
		};
	}

	if (value instanceof ArrayBuffer) {
		return {
			__type: "buffer",
			value: Buffer.from(value).toString("base64"),
		};
	}

	if (ArrayBuffer.isView(value)) {
		return {
			__type: "buffer",
			value: Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString("base64"),
		};
	}

	if (Array.isArray(value)) {
		return value.map((entry) => serializeBridgeValue(entry));
	}

	if (
		value &&
		typeof value === "object" &&
		"type" in value &&
		(((value as { type?: unknown }).type === "public" ||
			(value as { type?: unknown }).type === "private" ||
			(value as { type?: unknown }).type === "secret")) &&
		typeof (value as { export?: unknown }).export === "function"
	) {
		return {
			__type: "keyObject",
			value: serializeAnyKeyObject(value as any),
		};
	}

	if (value && typeof value === "object") {
		return Object.fromEntries(
			Object.entries(value).flatMap(([key, entry]) =>
				entry === undefined ? [] : [[key, serializeBridgeValue(entry)]],
			),
		);
	}

	return String(value);
}

function deserializeSandboxKeyObject(serialized: SerializedSandboxKeyObject): any {
	if (serialized.type === "secret") {
		return createSecretKey(Buffer.from(serialized.raw || "", "base64"));
	}

	if (serialized.type === "private") {
		return createPrivateKey(String(serialized.pem || ""));
	}

	return createPublicKey(String(serialized.pem || ""));
}

function deserializeBridgeValue(value: SerializedBridgeValue): unknown {
	if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
		return value;
	}

	if (Array.isArray(value)) {
		return value.map((entry) => deserializeBridgeValue(entry));
	}

	if ("__type" in value) {
		if (value.__type === "buffer") {
			return Buffer.from((value as { value: string }).value, "base64");
		}
		if (value.__type === "bigint") {
			return BigInt((value as { value: string }).value);
		}
		if (value.__type === "keyObject") {
			return deserializeSandboxKeyObject((value as { value: SerializedSandboxKeyObject }).value);
		}
	}

	return Object.fromEntries(
		Object.entries(value).map(([key, entry]) => [key, deserializeBridgeValue(entry)]),
	);
}

function parseSerializedOptions(
	optionsJson: unknown,
): unknown {
	const parsed = JSON.parse(String(optionsJson)) as {
		hasOptions?: boolean;
		options?: SerializedBridgeValue;
	};
	if (!parsed || parsed.hasOptions !== true) {
		return undefined;
	}
	return deserializeBridgeValue(parsed.options ?? null);
}

function serializeDispatchError(error: unknown): SerializedDispatchError {
	if (error instanceof Error) {
		const withCode = error as Error & {
			code?: unknown;
		};
		return {
			message: error.message,
			name: error.name,
			code: typeof withCode.code === "string" ? withCode.code : undefined,
			stack: error.stack,
		};
	}

	return {
		message: String(error),
		name: "Error",
	};
}

function restoreDispatchArgument(value: unknown): unknown {
	if (!value || typeof value !== "object") {
		return value;
	}

	if (
		(value as { __secureExecDispatchType?: unknown }).__secureExecDispatchType ===
		"undefined"
	) {
		return undefined;
	}

	if (Array.isArray(value)) {
		return value.map((entry) => restoreDispatchArgument(entry));
	}

	return Object.fromEntries(
		Object.entries(value).map(([key, entry]) => [key, restoreDispatchArgument(entry)]),
	);
}

function normalizeBridgeAlgorithm(algorithm: unknown): string | null {
	if (algorithm === null || algorithm === undefined || algorithm === "") {
		return null;
	}

	return String(algorithm);
}

interface BridgeCryptoKeyData {
	type: "public" | "private" | "secret";
	extractable: boolean;
	algorithm: Record<string, unknown>;
	usages: string[];
	_pem?: string;
	_jwk?: Record<string, unknown>;
	_raw?: string;
	_sourceKeyObjectData?: Record<string, unknown>;
}

function decodeBridgeBuffer(data: unknown): Buffer {
	return Buffer.from(String(data), "base64");
}

function sanitizeJsonValue(value: unknown): unknown {
	if (typeof value === "bigint") {
		return Number(value);
	}
	if (Array.isArray(value)) {
		return value.map((entry) => sanitizeJsonValue(entry));
	}
	if (!value || typeof value !== "object") {
		return value;
	}
	return Object.fromEntries(
		Object.entries(value as Record<string, unknown>).map(([key, entry]) => [
			key,
			sanitizeJsonValue(entry),
		]),
	);
}

function serializeCryptoKeyDataFromKeyObject(
	keyObject: KeyObject,
	type: "public" | "private" | "secret",
	algorithm: Record<string, unknown>,
	extractable: boolean,
	usages: string[],
): BridgeCryptoKeyData {
	if (type === "secret") {
		return {
			type,
			algorithm,
			extractable,
			usages,
			_raw: keyObject.export().toString("base64"),
			_sourceKeyObjectData: {
				type: "secret",
				raw: keyObject.export().toString("base64"),
			},
		};
	}

	return {
		type,
		algorithm,
		extractable,
		usages,
		_pem:
			type === "private"
				? (keyObject.export({ type: "pkcs8", format: "pem" }) as string)
				: (keyObject.export({ type: "spki", format: "pem" }) as string),
		_sourceKeyObjectData: {
			type,
			pem:
				type === "private"
					? (keyObject.export({ type: "pkcs8", format: "pem" }) as string)
					: (keyObject.export({ type: "spki", format: "pem" }) as string),
			asymmetricKeyType: keyObject.asymmetricKeyType,
			asymmetricKeyDetails: sanitizeJsonValue(keyObject.asymmetricKeyDetails),
		},
	};
}

function deserializeCryptoKeyObject(key: BridgeCryptoKeyData): KeyObject {
	if (key.type === "secret") {
		return createSecretKey(decodeBridgeBuffer(key._raw));
	}

	return key.type === "private"
		? createPrivateKey(key._pem ?? "")
		: createPublicKey(key._pem ?? "");
}

function normalizeHmacLength(hashName: string, explicitLength?: unknown): number {
	if (typeof explicitLength === "number") {
		return explicitLength;
	}

	switch (hashName) {
		case "SHA-1":
		case "SHA-256":
			return 512;
		case "SHA-384":
		case "SHA-512":
			return 1024;
		default:
			return 512;
	}
}

function sliceDerivedBits(secret: Buffer, length: unknown): Buffer {
	if (length === undefined || length === null) {
		return Buffer.from(secret);
	}

	const requestedBits = Number(length);
	const maxBits = secret.byteLength * 8;
	if (requestedBits > maxBits) {
		throw new Error("derived bit length is too small");
	}

	const requestedBytes = Math.ceil(requestedBits / 8);
	const derived = Buffer.from(secret.subarray(0, requestedBytes));
	const remainder = requestedBits % 8;
	if (remainder !== 0 && derived.length > 0) {
		derived[derived.length - 1] &= 0xff << (8 - remainder);
	}
	return derived;
}

function deriveSecretKeyData(
	derivedKeyAlgorithm: Record<string, unknown> | string,
	extractable: boolean,
	usages: string[],
	secret: Buffer,
): BridgeCryptoKeyData {
	const normalizedAlgorithm =
		typeof derivedKeyAlgorithm === "string"
			? { name: derivedKeyAlgorithm }
			: derivedKeyAlgorithm;
	const algorithmName = String(normalizedAlgorithm.name ?? "");
	if (algorithmName === "HMAC") {
		const hashName =
			typeof normalizedAlgorithm.hash === "string"
				? normalizedAlgorithm.hash
				: String((normalizedAlgorithm.hash as { name?: string } | undefined)?.name ?? "");
		const lengthBits = normalizeHmacLength(hashName, normalizedAlgorithm.length);
		const keyBytes = Buffer.from(secret.subarray(0, Math.ceil(lengthBits / 8)));
		return serializeCryptoKeyDataFromKeyObject(
			createSecretKey(keyBytes),
			"secret",
			{
				name: "HMAC",
				hash: { name: hashName },
				length: lengthBits,
			},
			extractable,
			usages,
		);
	}

	const lengthBits = Number(normalizedAlgorithm.length ?? secret.byteLength * 8);
	const keyBytes = Buffer.from(secret.subarray(0, Math.ceil(lengthBits / 8)));
	return serializeCryptoKeyDataFromKeyObject(
		createSecretKey(keyBytes),
		"secret",
		{
			...normalizedAlgorithm,
			length: lengthBits,
		},
		extractable,
		usages,
	);
}

function resolveDerivedKeyLengthBits(
	derivedKeyAlgorithm: Record<string, unknown> | string,
	fallbackBits: number,
): number {
	const normalizedAlgorithm =
		typeof derivedKeyAlgorithm === "string"
			? { name: derivedKeyAlgorithm }
			: derivedKeyAlgorithm;
	if (typeof normalizedAlgorithm.length === "number") {
		return normalizedAlgorithm.length;
	}
	if (normalizedAlgorithm.name === "HMAC") {
		const hashName =
			typeof normalizedAlgorithm.hash === "string"
				? normalizedAlgorithm.hash
				: String((normalizedAlgorithm.hash as { name?: string } | undefined)?.name ?? "");
		return normalizeHmacLength(hashName);
	}
	return fallbackBits;
}

/**
 * Build crypto bridge handlers.
 *
 * All handler functions are plain functions (no ivm.Reference wrapping).
 * The V8 runtime registers these by name on the V8 global.
 * Call dispose() when the execution ends to clear stateful cipher sessions.
 */
export function buildCryptoBridgeHandlers(): CryptoBridgeResult {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;

	// Stateful cipher sessions — tracks cipher/decipher instances between
	// create/update/final bridge calls (needed for ssh2 streaming AES-GCM).
	const cipherSessions = new Map<number, CipherSession>();
	let nextCipherSessionId = 1;
	const diffieHellmanSessions = new Map<number, DiffieHellmanSession>();
	let nextDiffieHellmanSessionId = 1;

	// Secure randomness — cap matches Web Crypto API spec (65536 bytes).
	handlers[K.cryptoRandomFill] = (byteLength: unknown) => {
		const len = Number(byteLength);
		if (len > 65536) {
			throw new RangeError(
				`The ArrayBufferView's byte length (${len}) exceeds the number of bytes of entropy available via this API (65536)`,
			);
		}
		const buffer = Buffer.allocUnsafe(len);
		randomFillSync(buffer);
		return buffer.toString("base64");
	};
	handlers[K.cryptoRandomUuid] = () => randomUUID();

	// createHash — guest accumulates update() data, sends base64 to host for digest.
	handlers[K.cryptoHashDigest] = (algorithm: unknown, dataBase64: unknown) => {
		const data = Buffer.from(String(dataBase64), "base64");
		const hash = createHash(String(algorithm));
		hash.update(data);
		return hash.digest("base64");
	};

	// createHmac — guest accumulates update() data, sends base64 to host for HMAC digest.
	handlers[K.cryptoHmacDigest] = (algorithm: unknown, keyBase64: unknown, dataBase64: unknown) => {
		const key = Buffer.from(String(keyBase64), "base64");
		const data = Buffer.from(String(dataBase64), "base64");
		const hmac = createHmac(String(algorithm), key);
		hmac.update(data);
		return hmac.digest("base64");
	};

	// pbkdf2Sync — derive key from password + salt.
	handlers[K.cryptoPbkdf2] = (
		passwordBase64: unknown,
		saltBase64: unknown,
		iterations: unknown,
		keylen: unknown,
		digest: unknown,
	) => {
		const password = Buffer.from(String(passwordBase64), "base64");
		const salt = Buffer.from(String(saltBase64), "base64");
		return pbkdf2Sync(
			password,
			salt,
			Number(iterations),
			Number(keylen),
			String(digest),
		).toString("base64");
	};

	// scryptSync — derive key from password + salt with tunable cost params.
	handlers[K.cryptoScrypt] = (
		passwordBase64: unknown,
		saltBase64: unknown,
		keylen: unknown,
		optionsJson: unknown,
	) => {
		const password = Buffer.from(String(passwordBase64), "base64");
		const salt = Buffer.from(String(saltBase64), "base64");
		const options = JSON.parse(String(optionsJson));
		return scryptSync(password, salt, Number(keylen), options).toString(
			"base64",
		);
	};

	// createCipheriv — guest accumulates update() data, sends base64 to host for encryption.
	// Returns JSON with data (and authTag for GCM modes).
	handlers[K.cryptoCipheriv] = (
		algorithm: unknown,
		keyBase64: unknown,
		ivBase64: unknown,
		dataBase64: unknown,
		optionsJson?: unknown,
	) => {
		const key = Buffer.from(String(keyBase64), "base64");
		const iv = ivBase64 === null ? null : Buffer.from(String(ivBase64), "base64");
		const data = Buffer.from(String(dataBase64), "base64");
		const options = optionsJson ? JSON.parse(String(optionsJson)) : {};
		const cipher = createCipheriv(String(algorithm), key, iv, (
			options.authTagLength !== undefined
				? { authTagLength: options.authTagLength }
				: undefined
		) as any) as any;
		if (options.validateOnly) {
			return JSON.stringify({ data: "" });
		}
		if (options.aad) {
			cipher.setAAD(Buffer.from(String(options.aad), "base64"), options.aadOptions);
		}
		if (options.autoPadding !== undefined) {
			cipher.setAutoPadding(Boolean(options.autoPadding));
		}
		const encrypted = Buffer.concat([cipher.update(data), cipher.final()]);
		const isAead = /-(gcm|ccm)$/i.test(String(algorithm));
		if (isAead) {
			return JSON.stringify({
				data: encrypted.toString("base64"),
				authTag: cipher.getAuthTag().toString("base64"),
			});
		}
		return JSON.stringify({ data: encrypted.toString("base64") });
	};

	// createDecipheriv — guest accumulates update() data, sends base64 to host for decryption.
	// Accepts optionsJson with authTag for GCM modes.
	handlers[K.cryptoDecipheriv] = (
		algorithm: unknown,
		keyBase64: unknown,
		ivBase64: unknown,
		dataBase64: unknown,
		optionsJson: unknown,
	) => {
		const key = Buffer.from(String(keyBase64), "base64");
		const iv = ivBase64 === null ? null : Buffer.from(String(ivBase64), "base64");
		const data = Buffer.from(String(dataBase64), "base64");
		const options = JSON.parse(String(optionsJson));
		const decipher = createDecipheriv(String(algorithm), key, iv, (
			options.authTagLength !== undefined
				? { authTagLength: options.authTagLength }
				: undefined
		) as any) as any;
		if (options.validateOnly) {
			return "";
		}
		const isAead = /-(gcm|ccm)$/i.test(String(algorithm));
		if (isAead && options.authTag) {
			decipher.setAuthTag(Buffer.from(options.authTag, "base64"));
		}
		if (options.aad) {
			decipher.setAAD(Buffer.from(String(options.aad), "base64"), options.aadOptions);
		}
		if (options.autoPadding !== undefined) {
			decipher.setAutoPadding(Boolean(options.autoPadding));
		}
		return Buffer.concat([decipher.update(data), decipher.final()]).toString(
			"base64",
		);
	};

	// Stateful cipheriv create — opens a cipher or decipher session on the host.
	// mode: "cipher" | "decipher"; returns sessionId.
	handlers[K.cryptoCipherivCreate] = (
		mode: unknown,
		algorithm: unknown,
		keyBase64: unknown,
		ivBase64: unknown,
		optionsJson: unknown,
	) => {
		const algo = String(algorithm);
		const key = Buffer.from(String(keyBase64), "base64");
		const iv = ivBase64 === null ? null : Buffer.from(String(ivBase64), "base64");
		const options = optionsJson ? JSON.parse(String(optionsJson)) : {};
		const isAead = /-(gcm|ccm)$/i.test(algo);

		let instance: Cipher | Decipher;
		if (String(mode) === "decipher") {
			const d = createDecipheriv(algo, key, iv, (
				options.authTagLength !== undefined
					? { authTagLength: options.authTagLength }
					: undefined
			) as any) as any;
			if (isAead && options.authTag) {
				d.setAuthTag(Buffer.from(options.authTag, "base64"));
			}
			instance = d;
		} else {
			instance = createCipheriv(algo, key, iv, (
				options.authTagLength !== undefined
					? { authTagLength: options.authTagLength }
					: undefined
			) as any) as any;
		}

		const sessionId = nextCipherSessionId++;
		cipherSessions.set(sessionId, { cipher: instance, algorithm: algo });
		return sessionId;
	};

	// Stateful cipheriv update — feeds data into an open session, returns partial result.
	handlers[K.cryptoCipherivUpdate] = (
		sessionId: unknown,
		dataBase64: unknown,
	) => {
		const id = Number(sessionId);
		const session = cipherSessions.get(id);
		if (!session) throw new Error(`Cipher session ${id} not found`);
		const data = Buffer.from(String(dataBase64), "base64");
		const result = session.cipher.update(data);
		return result.toString("base64");
	};

	// Stateful cipheriv final — finalizes session, returns last block + authTag for GCM.
	// Removes session from map.
	handlers[K.cryptoCipherivFinal] = (sessionId: unknown) => {
		const id = Number(sessionId);
		const session = cipherSessions.get(id);
		if (!session) throw new Error(`Cipher session ${id} not found`);
		cipherSessions.delete(id);
		const final = session.cipher.final();
		const isAead = /-(gcm|ccm)$/i.test(session.algorithm);
		if (isAead) {
			const authTag = (session.cipher as any).getAuthTag?.();
			return JSON.stringify({
				data: final.toString("base64"),
				authTag: authTag ? authTag.toString("base64") : undefined,
			});
		}
		return JSON.stringify({ data: final.toString("base64") });
	};

	// sign — host signs data with a PEM private key.
	handlers[K.cryptoSign] = (
		algorithm: unknown,
		dataBase64: unknown,
		keyJson: unknown,
	) => {
		const data = Buffer.from(String(dataBase64), "base64");
		const key = deserializeBridgeValue(JSON.parse(String(keyJson)) as SerializedBridgeValue) as any;
		const signature = sign(normalizeBridgeAlgorithm(algorithm), data, key);
		return signature.toString("base64");
	};

	// verify — host verifies signature with a PEM public key.
	handlers[K.cryptoVerify] = (
		algorithm: unknown,
		dataBase64: unknown,
		keyJson: unknown,
		signatureBase64: unknown,
	) => {
		const data = Buffer.from(String(dataBase64), "base64");
		const key = deserializeBridgeValue(JSON.parse(String(keyJson)) as SerializedBridgeValue) as any;
		const signature = Buffer.from(String(signatureBase64), "base64");
		return verify(normalizeBridgeAlgorithm(algorithm), data, key, signature);
	};

	// Asymmetric encrypt/decrypt — use real Node crypto so DER inputs, encrypted
	// PEM options bags, and sandbox KeyObject handles all follow host semantics.
	handlers[K.cryptoAsymmetricOp] = (
		operation: unknown,
		keyJson: unknown,
		dataBase64: unknown,
	) => {
		const key = deserializeBridgeValue(JSON.parse(String(keyJson)) as SerializedBridgeValue) as any;
		const data = Buffer.from(String(dataBase64), "base64");
		switch (String(operation)) {
			case "publicEncrypt":
				return publicEncrypt(key, data).toString("base64");
			case "privateDecrypt":
				return privateDecrypt(key, data).toString("base64");
			case "privateEncrypt":
				return privateEncrypt(key, data).toString("base64");
			case "publicDecrypt":
				return publicDecrypt(key, data).toString("base64");
			default:
				throw new Error(`Unsupported asymmetric crypto operation: ${String(operation)}`);
		}
	};

	// createPublicKey/createPrivateKey — import through host crypto so metadata
	// like asymmetricKeyType/asymmetricKeyDetails survives reconstruction.
	handlers[K.cryptoCreateKeyObject] = (
		operation: unknown,
		keyJson: unknown,
	) => {
		const key = deserializeBridgeValue(JSON.parse(String(keyJson)) as SerializedBridgeValue) as any;
		switch (String(operation)) {
			case "createPrivateKey":
				return JSON.stringify(serializeAnyKeyObject(createPrivateKey(key)));
			case "createPublicKey":
				return JSON.stringify(serializeAnyKeyObject(createPublicKey(key)));
			default:
				throw new Error(`Unsupported key creation operation: ${String(operation)}`);
		}
	};

	// generateKeyPairSync — host generates key pair, preserving requested encodings.
	// For KeyObject output, serialize PEM + metadata so the isolate can recreate a
	// Node-compatible KeyObject surface.
	handlers[K.cryptoGenerateKeyPairSync] = (
		type: unknown,
		optionsJson: unknown,
	) => {
		const options = parseSerializedOptions(optionsJson);
		const encodingOptions = options as
			| {
					publicKeyEncoding?: unknown;
					privateKeyEncoding?: unknown;
			  }
			| undefined;
		const hasExplicitEncoding =
			encodingOptions &&
			(encodingOptions.publicKeyEncoding || encodingOptions.privateKeyEncoding);
		const { publicKey, privateKey } = generateKeyPairSync(type as any, options as any);

		if (hasExplicitEncoding) {
			return JSON.stringify({
				publicKey: serializeKeyValue(publicKey as unknown),
				privateKey: serializeKeyValue(privateKey as unknown),
			});
		}

		return JSON.stringify({
			publicKey: serializeSandboxKeyObject(publicKey as any),
			privateKey: serializeSandboxKeyObject(privateKey as any),
		});
	};

	// generateKeySync — host generates symmetric KeyObject values with native
	// validation so length/error semantics match Node.
	handlers[K.cryptoGenerateKeySync] = (
		type: unknown,
		optionsJson: unknown,
	) => {
		const options = parseSerializedOptions(optionsJson);
		return JSON.stringify(
			serializeAnyKeyObject(generateKeySync(type as any, options as any)),
		);
	};

	// generatePrimeSync — host generates prime material so bigint/add/rem options
	// follow Node semantics instead of polyfill approximations.
	handlers[K.cryptoGeneratePrimeSync] = (
		size: unknown,
		optionsJson: unknown,
	) => {
		const options = parseSerializedOptions(optionsJson);
		const prime =
			options === undefined
				? generatePrimeSync(size as any)
				: generatePrimeSync(size as any, options as any);
		return JSON.stringify(serializeBridgeValue(prime));
	};

	// Diffie-Hellman/ECDH — keep native host objects alive by session id so
	// sandbox calls preserve Node's return values, validation, and stateful key material.
	handlers[K.cryptoDiffieHellman] = (optionsJson: unknown) => {
		const options = deserializeBridgeValue(
			JSON.parse(String(optionsJson)) as SerializedBridgeValue,
		) as Parameters<typeof diffieHellman>[0];
		return JSON.stringify(
			serializeBridgeValue(diffieHellman(options)),
		);
	};

	handlers[K.cryptoDiffieHellmanGroup] = (name: unknown) => {
		const group = getDiffieHellman(String(name));
		return JSON.stringify({
			prime: serializeBridgeValue(group.getPrime()),
			generator: serializeBridgeValue(group.getGenerator()),
		});
	};

	handlers[K.cryptoDiffieHellmanSessionCreate] = (requestJson: unknown) => {
		const request = JSON.parse(String(requestJson)) as {
			type: "dh" | "group" | "ecdh";
			name?: string;
			args?: SerializedBridgeValue[];
		};
		const args = (request.args ?? []).map((value) =>
			deserializeBridgeValue(value),
		);

		let session: DiffieHellmanSession;
		switch (request.type) {
			case "dh":
				session = createDiffieHellman(...(args as Parameters<typeof createDiffieHellman>));
				break;
			case "group":
				session = getDiffieHellman(String(request.name));
				break;
			case "ecdh":
				session = createECDH(String(request.name));
				break;
			default:
				throw new Error(`Unsupported Diffie-Hellman session type: ${String((request as any).type)}`);
		}

		const sessionId = nextDiffieHellmanSessionId++;
		diffieHellmanSessions.set(sessionId, session);
		return sessionId;
	};

	handlers[K.cryptoDiffieHellmanSessionCall] = (
		sessionId: unknown,
		requestJson: unknown,
	) => {
		const session = diffieHellmanSessions.get(Number(sessionId));
		if (!session) {
			throw new Error(`Diffie-Hellman session ${String(sessionId)} not found`);
		}

		const request = JSON.parse(String(requestJson)) as {
			method: string;
			args?: SerializedBridgeValue[];
		};
		const args = (request.args ?? []).map((value) =>
			deserializeBridgeValue(value),
		);

		const sessionRecord = session as unknown as Record<string, unknown>;

		if (request.method === "verifyError") {
			return JSON.stringify({
				result: typeof sessionRecord.verifyError === "number" ? sessionRecord.verifyError : undefined,
				hasResult: typeof sessionRecord.verifyError === "number",
			});
		}

		const method = sessionRecord[request.method];
		if (typeof method !== "function") {
			throw new Error(`Unsupported Diffie-Hellman method: ${request.method}`);
		}

		const result = (method as (...callArgs: unknown[]) => unknown).apply(session, args);
		return JSON.stringify({
			result: result === undefined ? null : serializeBridgeValue(result),
			hasResult: result !== undefined,
		});
	};

	// crypto.subtle — single dispatcher for all Web Crypto API operations.
	// Guest-side SandboxSubtle serializes each call as JSON { op, ... }.
	handlers[K.cryptoSubtle] = (opJson: unknown) => {
		const req = JSON.parse(String(opJson));
		const normalizeHash = (h: string | { name: string }): string => {
			const n = typeof h === "string" ? h : h.name;
			return n.toLowerCase().replace("-", "");
		};
		switch (req.op) {
			case "digest": {
				const algo = normalizeHash(req.algorithm);
				const data = Buffer.from(req.data, "base64");
				return JSON.stringify({
					data: createHash(algo).update(data).digest("base64"),
				});
			}
			case "generateKey": {
				const algoName = req.algorithm.name;
				if (
					algoName === "AES-GCM" ||
					algoName === "AES-CBC" ||
					algoName === "AES-CTR" ||
					algoName === "AES-KW"
				) {
					const keyBytes = Buffer.allocUnsafe(req.algorithm.length / 8);
					randomFillSync(keyBytes);
					return JSON.stringify({
						key: serializeCryptoKeyDataFromKeyObject(
							createSecretKey(keyBytes),
							"secret",
							req.algorithm,
							req.extractable,
							req.usages,
						),
					});
				}
				if (algoName === "HMAC") {
					const hashName =
						typeof req.algorithm.hash === "string"
							? req.algorithm.hash
							: req.algorithm.hash.name;
					const len = normalizeHmacLength(hashName, req.algorithm.length) / 8;
					const keyBytes = Buffer.allocUnsafe(len);
					randomFillSync(keyBytes);
					return JSON.stringify({
						key: serializeCryptoKeyDataFromKeyObject(
							createSecretKey(keyBytes),
							"secret",
							{
								...req.algorithm,
								hash: { name: hashName },
								length: len * 8,
							},
							req.extractable,
							req.usages,
						),
					});
				}
				if (
					algoName === "RSASSA-PKCS1-v1_5" ||
					algoName === "RSA-OAEP" ||
					algoName === "RSA-PSS"
				) {
					let publicExponent = 65537;
					if (req.algorithm.publicExponent) {
						const expBytes = Buffer.from(
							req.algorithm.publicExponent,
							"base64",
						);
						publicExponent = 0;
						for (const b of expBytes) {
							publicExponent = (publicExponent << 8) | b;
						}
					}
					const { publicKey, privateKey } = generateKeyPairSync("rsa", {
						modulusLength: req.algorithm.modulusLength || 2048,
						publicExponent,
						publicKeyEncoding: {
							type: "spki" as const,
							format: "pem" as const,
						},
						privateKeyEncoding: {
							type: "pkcs8" as const,
							format: "pem" as const,
						},
					});
					const publicKeyObject = createPublicKey(publicKey);
					const privateKeyObject = createPrivateKey(privateKey);
					return JSON.stringify({
						publicKey: serializeCryptoKeyDataFromKeyObject(
							publicKeyObject,
							"public",
							req.algorithm,
							req.extractable,
							req.usages.filter((u: string) =>
								["verify", "encrypt", "wrapKey"].includes(u),
							),
						),
						privateKey: serializeCryptoKeyDataFromKeyObject(
							privateKeyObject,
							"private",
							req.algorithm,
							req.extractable,
							req.usages.filter((u: string) =>
								["sign", "decrypt", "unwrapKey"].includes(u),
							),
						),
					});
				}
				if (algoName === "ECDSA" || algoName === "ECDH") {
					const { publicKey, privateKey } = generateKeyPairSync("ec", {
						namedCurve: String(req.algorithm.namedCurve),
						publicKeyEncoding: { type: "spki", format: "pem" },
						privateKeyEncoding: { type: "pkcs8", format: "pem" },
					});
					return JSON.stringify({
						publicKey: serializeCryptoKeyDataFromKeyObject(
							createPublicKey(publicKey),
							"public",
							{ ...req.algorithm, name: algoName },
							req.extractable,
							req.usages.filter((u: string) =>
								algoName === "ECDSA"
									? ["verify"].includes(u)
									: ["deriveBits", "deriveKey"].includes(u),
							),
						),
						privateKey: serializeCryptoKeyDataFromKeyObject(
							createPrivateKey(privateKey),
							"private",
							{ ...req.algorithm, name: algoName },
							req.extractable,
							req.usages.filter((u: string) =>
								algoName === "ECDSA"
									? ["sign"].includes(u)
									: ["deriveBits", "deriveKey"].includes(u),
							),
						),
					});
				}
				if (["Ed25519", "Ed448", "X25519", "X448"].includes(algoName)) {
					const keyPair =
						algoName === "Ed25519"
							? generateKeyPairSync("ed25519")
							: algoName === "Ed448"
								? generateKeyPairSync("ed448")
								: algoName === "X25519"
									? generateKeyPairSync("x25519")
									: generateKeyPairSync("x448");
					const { publicKey, privateKey } = keyPair;
					return JSON.stringify({
						publicKey: serializeCryptoKeyDataFromKeyObject(
							publicKey,
							"public",
							{ name: algoName },
							req.extractable,
							req.usages.filter((u: string) =>
								algoName.startsWith("Ed")
									? ["verify"].includes(u)
									: ["deriveBits", "deriveKey"].includes(u),
							),
						),
						privateKey: serializeCryptoKeyDataFromKeyObject(
							privateKey,
							"private",
							{ name: algoName },
							req.extractable,
							req.usages.filter((u: string) =>
								algoName.startsWith("Ed")
									? ["sign"].includes(u)
									: ["deriveBits", "deriveKey"].includes(u),
							),
						),
					});
				}
				throw new Error(`Unsupported key algorithm: ${algoName}`);
			}
			case "importKey": {
				const { format, keyData, algorithm, extractable, usages } = req;
				if (format === "raw") {
					return JSON.stringify({
						key: serializeCryptoKeyDataFromKeyObject(
							createSecretKey(Buffer.from(keyData, "base64")),
							"secret",
							algorithm.name === "HMAC" && !algorithm.length
								? {
										...algorithm,
										hash:
											typeof algorithm.hash === "string"
												? { name: algorithm.hash }
												: algorithm.hash,
										length: Buffer.from(keyData, "base64").byteLength * 8,
								  }
								: algorithm,
							extractable,
							usages,
						),
					});
				}
				if (format === "jwk") {
					const jwk =
						typeof keyData === "string" ? JSON.parse(keyData) : keyData;
					if (jwk.kty === "oct") {
						const raw = Buffer.from(jwk.k, "base64url");
						return JSON.stringify({
							key: serializeCryptoKeyDataFromKeyObject(
								createSecretKey(raw),
								"secret",
								algorithm,
								extractable,
								usages,
							),
						});
					}
					if (jwk.d) {
						const keyObj = createPrivateKey({ key: jwk, format: "jwk" });
						const pem = keyObj.export({
							type: "pkcs8",
							format: "pem",
						}) as string;
						return JSON.stringify({
							key: serializeCryptoKeyDataFromKeyObject(
								createPrivateKey(pem),
								"private",
								algorithm,
								extractable,
								usages,
							),
						});
					}
					const keyObj = createPublicKey({ key: jwk, format: "jwk" });
					const pem = keyObj.export({ type: "spki", format: "pem" }) as string;
					return JSON.stringify({
						key: serializeCryptoKeyDataFromKeyObject(
							createPublicKey(pem),
							"public",
							algorithm,
							extractable,
							usages,
						),
					});
				}
				if (format === "pkcs8") {
					const keyBuf = Buffer.from(keyData, "base64");
					const keyObj = createPrivateKey({
						key: keyBuf,
						format: "der",
						type: "pkcs8",
					});
					const pem = keyObj.export({
						type: "pkcs8",
						format: "pem",
					}) as string;
					return JSON.stringify({
						key: serializeCryptoKeyDataFromKeyObject(
							createPrivateKey(pem),
							"private",
							algorithm,
							extractable,
							usages,
						),
					});
				}
				if (format === "spki") {
					const keyBuf = Buffer.from(keyData, "base64");
					const keyObj = createPublicKey({
						key: keyBuf,
						format: "der",
						type: "spki",
					});
					const pem = keyObj.export({ type: "spki", format: "pem" }) as string;
					return JSON.stringify({
						key: serializeCryptoKeyDataFromKeyObject(
							createPublicKey(pem),
							"public",
							algorithm,
							extractable,
							usages,
						),
					});
				}
				throw new Error(`Unsupported import format: ${format}`);
			}
			case "exportKey": {
				const { format, key } = req;
				if (format === "raw") {
					if (!key._raw)
						throw new Error("Cannot export asymmetric key as raw");
					return JSON.stringify({
						data: key._raw,
					});
				}
				if (format === "jwk") {
					if (key._raw) {
						const raw = Buffer.from(key._raw, "base64");
						return JSON.stringify({
							jwk: {
								kty: "oct",
								k: raw.toString("base64url"),
								ext: key.extractable,
								key_ops: key.usages,
							},
						});
					}
					const keyObj =
						key.type === "private"
							? createPrivateKey(key._pem)
							: createPublicKey(key._pem);
					return JSON.stringify({
						jwk: keyObj.export({ format: "jwk" }),
					});
				}
				if (format === "pkcs8") {
					if (key.type !== "private")
						throw new Error("Cannot export non-private key as pkcs8");
					const keyObj = createPrivateKey(key._pem);
					const der = keyObj.export({
						type: "pkcs8",
						format: "der",
					}) as Buffer;
					return JSON.stringify({ data: der.toString("base64") });
				}
				if (format === "spki") {
					const keyObj =
						key.type === "private"
							? createPublicKey(createPrivateKey(key._pem))
							: createPublicKey(key._pem);
					const der = keyObj.export({
						type: "spki",
						format: "der",
					}) as Buffer;
					return JSON.stringify({ data: der.toString("base64") });
				}
				throw new Error(`Unsupported export format: ${format}`);
			}
			case "encrypt": {
				const { algorithm, key, data } = req;
				const rawKey = Buffer.from(key._raw, "base64");
				const plaintext = Buffer.from(data, "base64");
				const algoName = algorithm.name;
				if (algoName === "AES-GCM") {
					const iv = Buffer.from(algorithm.iv, "base64");
					const tagLength = (algorithm.tagLength || 128) / 8;
					const cipher = createCipheriv(
						`aes-${rawKey.length * 8}-gcm` as any,
						rawKey,
						iv,
						{ authTagLength: tagLength } as any,
					) as any;
					if (algorithm.additionalData) {
						cipher.setAAD(Buffer.from(algorithm.additionalData, "base64"));
					}
					const encrypted = Buffer.concat([
						cipher.update(plaintext),
						cipher.final(),
					]);
					const authTag = cipher.getAuthTag();
					return JSON.stringify({
						data: Buffer.concat([encrypted, authTag]).toString("base64"),
					});
				}
				if (algoName === "AES-CBC") {
					const iv = Buffer.from(algorithm.iv, "base64");
					const cipher = createCipheriv(
						`aes-${rawKey.length * 8}-cbc` as any,
						rawKey,
						iv,
					);
					const encrypted = Buffer.concat([
						cipher.update(plaintext),
						cipher.final(),
					]);
					return JSON.stringify({ data: encrypted.toString("base64") });
				}
				throw new Error(`Unsupported encrypt algorithm: ${algoName}`);
			}
			case "decrypt": {
				const { algorithm, key, data } = req;
				const rawKey = Buffer.from(key._raw, "base64");
				const ciphertext = Buffer.from(data, "base64");
				const algoName = algorithm.name;
				if (algoName === "AES-GCM") {
					const iv = Buffer.from(algorithm.iv, "base64");
					const tagLength = (algorithm.tagLength || 128) / 8;
					const encData = ciphertext.subarray(
						0,
						ciphertext.length - tagLength,
					);
					const authTag = ciphertext.subarray(
						ciphertext.length - tagLength,
					);
					const decipher = createDecipheriv(
						`aes-${rawKey.length * 8}-gcm` as any,
						rawKey,
						iv,
						{ authTagLength: tagLength } as any,
					) as any;
					decipher.setAuthTag(authTag);
					if (algorithm.additionalData) {
						decipher.setAAD(
							Buffer.from(algorithm.additionalData, "base64"),
						);
					}
					const decrypted = Buffer.concat([
						decipher.update(encData),
						decipher.final(),
					]);
					return JSON.stringify({ data: decrypted.toString("base64") });
				}
				if (algoName === "AES-CBC") {
					const iv = Buffer.from(algorithm.iv, "base64");
					const decipher = createDecipheriv(
						`aes-${rawKey.length * 8}-cbc` as any,
						rawKey,
						iv,
					);
					const decrypted = Buffer.concat([
						decipher.update(ciphertext),
						decipher.final(),
					]);
					return JSON.stringify({ data: decrypted.toString("base64") });
				}
				throw new Error(`Unsupported decrypt algorithm: ${algoName}`);
			}
			case "sign": {
				const { key, data, algorithm } = req;
				const dataBytes = Buffer.from(data, "base64");
				const algoName = key.algorithm.name;
				if (algoName === "HMAC") {
					const rawKey = Buffer.from(key._raw, "base64");
					const hashAlgo = normalizeHash(algorithm.hash ?? key.algorithm.hash);
					return JSON.stringify({
						data: createHmac(hashAlgo, rawKey)
							.update(dataBytes)
							.digest("base64"),
					});
				}
				if (algoName === "RSASSA-PKCS1-v1_5") {
					const hashAlgo = normalizeHash(key.algorithm.hash);
					const pkey = createPrivateKey(key._pem);
					return JSON.stringify({
						data: sign(hashAlgo, dataBytes, pkey).toString("base64"),
					});
				}
				if (algoName === "RSA-PSS") {
					const hashAlgo = normalizeHash(key.algorithm.hash);
					return JSON.stringify({
						data: sign(hashAlgo, dataBytes, {
							key: createPrivateKey(key._pem),
							padding: cryptoConstants.RSA_PKCS1_PSS_PADDING,
							saltLength: algorithm.saltLength,
						}).toString("base64"),
					});
				}
				if (algoName === "ECDSA") {
					const hashAlgo = normalizeHash(algorithm.hash ?? key.algorithm.hash);
					return JSON.stringify({
						data: sign(hashAlgo, dataBytes, createPrivateKey(key._pem)).toString("base64"),
					});
				}
				if (algoName === "Ed25519" || algoName === "Ed448") {
					if (
						algoName === "Ed448" &&
						algorithm.context &&
						Buffer.from(algorithm.context, "base64").byteLength > 0
					) {
						throw new Error("Non zero-length context is not yet supported");
					}
					return JSON.stringify({
						data: sign(null, dataBytes, createPrivateKey(key._pem)).toString("base64"),
					});
				}
				throw new Error(`Unsupported sign algorithm: ${algoName}`);
			}
			case "verify": {
				const { key, signature, data, algorithm } = req;
				const dataBytes = Buffer.from(data, "base64");
				const sigBytes = Buffer.from(signature, "base64");
				const algoName = key.algorithm.name;
				if (algoName === "HMAC") {
					const rawKey = Buffer.from(key._raw, "base64");
					const hashAlgo = normalizeHash(algorithm.hash ?? key.algorithm.hash);
					const expected = createHmac(hashAlgo, rawKey)
						.update(dataBytes)
						.digest();
					if (expected.length !== sigBytes.length)
						return JSON.stringify({ result: false });
					return JSON.stringify({
						result: timingSafeEqual(expected, sigBytes),
					});
				}
				if (algoName === "RSASSA-PKCS1-v1_5") {
					const hashAlgo = normalizeHash(key.algorithm.hash);
					const pkey = createPublicKey(key._pem);
					return JSON.stringify({
						result: verify(hashAlgo, dataBytes, pkey, sigBytes),
					});
				}
				if (algoName === "RSA-PSS") {
					const hashAlgo = normalizeHash(key.algorithm.hash);
					return JSON.stringify({
						result: verify(hashAlgo, dataBytes, {
							key: createPublicKey(key._pem),
							padding: cryptoConstants.RSA_PKCS1_PSS_PADDING,
							saltLength: algorithm.saltLength,
						}, sigBytes),
					});
				}
				if (algoName === "ECDSA") {
					const hashAlgo = normalizeHash(algorithm.hash ?? key.algorithm.hash);
					return JSON.stringify({
						result: verify(hashAlgo, dataBytes, createPublicKey(key._pem), sigBytes),
					});
				}
				if (algoName === "Ed25519" || algoName === "Ed448") {
					if (
						algoName === "Ed448" &&
						algorithm.context &&
						Buffer.from(algorithm.context, "base64").byteLength > 0
					) {
						throw new Error("Non zero-length context is not yet supported");
					}
					return JSON.stringify({
						result: verify(null, dataBytes, createPublicKey(key._pem), sigBytes),
					});
				}
				throw new Error(`Unsupported verify algorithm: ${algoName}`);
			}
			case "deriveBits": {
				const { algorithm, baseKey, length } = req;
				const algoName = algorithm.name;
				if (algoName === "PBKDF2") {
					const bitLength = Number(length);
					const byteLength = bitLength / 8;
					const password = Buffer.from(baseKey._raw, "base64");
					const salt = Buffer.from(algorithm.salt, "base64");
					const hash = normalizeHash(algorithm.hash);
					const derived = pbkdf2Sync(
						password,
						salt,
						algorithm.iterations,
						byteLength,
						hash,
					);
					return JSON.stringify({ data: derived.toString("base64") });
				}
				if (algoName === "HKDF") {
					const bitLength = Number(length);
					const byteLength = bitLength / 8;
					const ikm = Buffer.from(baseKey._raw, "base64");
					const salt = Buffer.from(algorithm.salt, "base64");
					const info = Buffer.from(algorithm.info, "base64");
					const hash = normalizeHash(algorithm.hash);
					const derived = Buffer.from(
						hkdfSync(hash, ikm, salt, info, byteLength),
					);
					return JSON.stringify({ data: derived.toString("base64") });
				}
				if (algoName === "ECDH" || algoName === "X25519" || algoName === "X448") {
					const secret = diffieHellman({
						privateKey: deserializeCryptoKeyObject(baseKey),
						publicKey: deserializeCryptoKeyObject(algorithm.public),
					});
					return JSON.stringify({
						data: sliceDerivedBits(secret, length).toString("base64"),
					});
				}
				throw new Error(`Unsupported deriveBits algorithm: ${algoName}`);
			}
			case "deriveKey": {
				const { algorithm, baseKey, derivedKeyAlgorithm, extractable, usages } = req;
				const algoName = algorithm.name;
				if (algoName === "PBKDF2") {
					const keyLengthBits = resolveDerivedKeyLengthBits(
						derivedKeyAlgorithm,
						Buffer.from(baseKey._raw, "base64").byteLength * 8,
					);
					const byteLength = keyLengthBits / 8;
					const password = Buffer.from(baseKey._raw, "base64");
					const salt = Buffer.from(algorithm.salt, "base64");
					const hash = normalizeHash(algorithm.hash);
					const derived = pbkdf2Sync(
						password,
						salt,
						algorithm.iterations,
						byteLength,
						hash,
					);
					return JSON.stringify({ key: deriveSecretKeyData(derivedKeyAlgorithm, extractable, usages, derived) });
				}
				if (algoName === "HKDF") {
					const keyLengthBits = resolveDerivedKeyLengthBits(
						derivedKeyAlgorithm,
						Buffer.from(baseKey._raw, "base64").byteLength * 8,
					);
					const byteLength = keyLengthBits / 8;
					const ikm = Buffer.from(baseKey._raw, "base64");
					const salt = Buffer.from(algorithm.salt, "base64");
					const info = Buffer.from(algorithm.info, "base64");
					const hash = normalizeHash(algorithm.hash);
					const derived = Buffer.from(
						hkdfSync(hash, ikm, salt, info, byteLength),
					);
					return JSON.stringify({ key: deriveSecretKeyData(derivedKeyAlgorithm, extractable, usages, derived) });
				}
				if (algoName === "ECDH" || algoName === "X25519" || algoName === "X448") {
					const secret = diffieHellman({
						privateKey: deserializeCryptoKeyObject(baseKey),
						publicKey: deserializeCryptoKeyObject(algorithm.public),
					});
					return JSON.stringify({
						key: deriveSecretKeyData(derivedKeyAlgorithm, extractable, usages, secret),
					});
				}
				throw new Error(`Unsupported deriveKey algorithm: ${algoName}`);
			}
			case "wrapKey": {
				const { format, key, wrappingKey, wrapAlgorithm } = req;
				const exported = JSON.parse(
					handlers[K.cryptoSubtle](
						JSON.stringify({
							op: "exportKey",
							format,
							key,
						}),
					) as string,
				) as { data?: string; jwk?: JsonWebKey };
				const keyData =
					format === "jwk"
						? Buffer.from(JSON.stringify(exported.jwk), "utf8")
						: decodeBridgeBuffer(exported.data);
				if (wrapAlgorithm.name === "AES-KW") {
					const wrappingBytes = decodeBridgeBuffer(wrappingKey._raw);
					const cipherName = `id-aes${wrappingBytes.byteLength * 8}-wrap`;
					const cipher = createCipheriv(
						cipherName as never,
						wrappingBytes,
						Buffer.alloc(8, 0xa6),
					);
					return JSON.stringify({
						data: Buffer.concat([cipher.update(keyData), cipher.final()]).toString("base64"),
					});
				}
				if (wrapAlgorithm.name === "RSA-OAEP") {
					return JSON.stringify({
						data: publicEncrypt(
							{
								key: createPublicKey(wrappingKey._pem),
								oaepHash: normalizeHash(wrappingKey.algorithm.hash),
								oaepLabel: wrapAlgorithm.label
									? decodeBridgeBuffer(wrapAlgorithm.label)
									: undefined,
							},
							keyData,
						).toString("base64"),
					});
				}
				if (
					wrapAlgorithm.name === "AES-CTR" ||
					wrapAlgorithm.name === "AES-CBC" ||
					wrapAlgorithm.name === "AES-GCM"
				) {
					const wrappingBytes = decodeBridgeBuffer(wrappingKey._raw);
					const algorithmName =
						wrapAlgorithm.name === "AES-CTR"
							? `aes-${wrappingBytes.byteLength * 8}-ctr`
							: wrapAlgorithm.name === "AES-CBC"
								? `aes-${wrappingBytes.byteLength * 8}-cbc`
								: `aes-${wrappingBytes.byteLength * 8}-gcm`;
					const iv =
						wrapAlgorithm.name === "AES-CTR"
							? decodeBridgeBuffer(wrapAlgorithm.counter)
							: decodeBridgeBuffer(wrapAlgorithm.iv);
					const cipher = createCipheriv(
						algorithmName as never,
						wrappingBytes,
						iv,
						wrapAlgorithm.name === "AES-GCM"
							? ({ authTagLength: (wrapAlgorithm.tagLength || 128) / 8 } as never)
							: undefined,
					) as Cipher & { setAAD?: (aad: Buffer) => void; getAuthTag?: () => Buffer };
					if (wrapAlgorithm.name === "AES-GCM" && wrapAlgorithm.additionalData) {
						cipher.setAAD?.(decodeBridgeBuffer(wrapAlgorithm.additionalData));
					}
					const encrypted = Buffer.concat([cipher.update(keyData), cipher.final()]);
					const payload =
						wrapAlgorithm.name === "AES-GCM"
							? Buffer.concat([encrypted, cipher.getAuthTag?.() ?? Buffer.alloc(0)])
							: encrypted;
					return JSON.stringify({ data: payload.toString("base64") });
				}
				throw new Error(`Unsupported wrap algorithm: ${wrapAlgorithm.name}`);
			}
			case "unwrapKey": {
				const {
					format,
					wrappedKey,
					unwrappingKey,
					unwrapAlgorithm,
					unwrappedKeyAlgorithm,
					extractable,
					usages,
				} = req;
				let unwrapped: Buffer;
				if (unwrapAlgorithm.name === "AES-KW") {
					const unwrappingBytes = decodeBridgeBuffer(unwrappingKey._raw);
					const cipherName = `id-aes${unwrappingBytes.byteLength * 8}-wrap`;
					const decipher = createDecipheriv(
						cipherName as never,
						unwrappingBytes,
						Buffer.alloc(8, 0xa6),
					);
					unwrapped = Buffer.concat([
						decipher.update(decodeBridgeBuffer(wrappedKey)),
						decipher.final(),
					]);
				} else if (unwrapAlgorithm.name === "RSA-OAEP") {
					unwrapped = privateDecrypt(
						{
							key: createPrivateKey(unwrappingKey._pem),
							oaepHash: normalizeHash(unwrappingKey.algorithm.hash),
							oaepLabel: unwrapAlgorithm.label
								? decodeBridgeBuffer(unwrapAlgorithm.label)
								: undefined,
						},
						decodeBridgeBuffer(wrappedKey),
					);
				} else if (
					unwrapAlgorithm.name === "AES-CTR" ||
					unwrapAlgorithm.name === "AES-CBC" ||
					unwrapAlgorithm.name === "AES-GCM"
				) {
					const unwrappingBytes = decodeBridgeBuffer(unwrappingKey._raw);
					const algorithmName =
						unwrapAlgorithm.name === "AES-CTR"
							? `aes-${unwrappingBytes.byteLength * 8}-ctr`
							: unwrapAlgorithm.name === "AES-CBC"
								? `aes-${unwrappingBytes.byteLength * 8}-cbc`
								: `aes-${unwrappingBytes.byteLength * 8}-gcm`;
					const iv =
						unwrapAlgorithm.name === "AES-CTR"
							? decodeBridgeBuffer(unwrapAlgorithm.counter)
							: decodeBridgeBuffer(unwrapAlgorithm.iv);
					const wrappedBytes = decodeBridgeBuffer(wrappedKey);
					const decipher = createDecipheriv(
						algorithmName as never,
						unwrappingBytes,
						iv,
						unwrapAlgorithm.name === "AES-GCM"
							? ({ authTagLength: (unwrapAlgorithm.tagLength || 128) / 8 } as never)
							: undefined,
					) as Decipher & {
						setAAD?: (aad: Buffer) => void;
						setAuthTag?: (tag: Buffer) => void;
					};
					let ciphertext = wrappedBytes;
					if (unwrapAlgorithm.name === "AES-GCM") {
						const tagLength = (unwrapAlgorithm.tagLength || 128) / 8;
						ciphertext = wrappedBytes.subarray(0, wrappedBytes.byteLength - tagLength);
						decipher.setAuthTag?.(wrappedBytes.subarray(wrappedBytes.byteLength - tagLength));
						if (unwrapAlgorithm.additionalData) {
							decipher.setAAD?.(decodeBridgeBuffer(unwrapAlgorithm.additionalData));
						}
					}
					unwrapped = Buffer.concat([decipher.update(ciphertext), decipher.final()]);
				} else {
					throw new Error(`Unsupported unwrap algorithm: ${unwrapAlgorithm.name}`);
				}
				return handlers[K.cryptoSubtle](
					JSON.stringify({
						op: "importKey",
						format,
						keyData:
							format === "jwk"
								? JSON.parse(unwrapped.toString("utf8"))
								: unwrapped.toString("base64"),
						algorithm: unwrappedKeyAlgorithm,
						extractable,
						usages,
					}),
				);
			}
			default:
				throw new Error(`Unsupported subtle operation: ${req.op}`);
		}
	};

	const dispose = () => {
		cipherSessions.clear();
		diffieHellmanSessions.clear();
	};

	return { handlers, dispose };
}

/** Dependencies for building net socket bridge handlers. */
export interface NetSocketBridgeDeps {
	/** Dispatch a socket event back to the guest (socketId, event, data?). */
	dispatch: (socketId: number, event: string, data?: string) => void;
	/** Kernel socket table — when provided, routes through kernel instead of host TCP. */
	socketTable?: import("@secure-exec/core").SocketTable;
	/** Process ID for kernel socket ownership. Required when socketTable is set. */
	pid?: number;
}

/** Result of building net socket bridge handlers — includes dispose for cleanup. */
export interface NetSocketBridgeResult {
	handlers: BridgeHandlers;
	dispose: () => void;
}

/**
 * Build net socket bridge handlers.
 *
 * All TCP operations route through kernel sockets (loopback or external via
 * the host adapter).
 * Call dispose() when the execution ends to destroy all open sockets.
 */
export function buildNetworkSocketBridgeHandlers(
	deps: NetSocketBridgeDeps,
): NetSocketBridgeResult {
	const { socketTable, pid } = deps;
	if (!socketTable || pid === undefined) {
		throw new Error("buildNetworkSocketBridgeHandlers requires a kernel socketTable and pid");
	}
	return buildKernelSocketBridgeHandlers(deps.dispatch, socketTable, pid);
}

/**
 * Build bridge handlers that route net socket operations through the
 * kernel SocketTable. Data flows through kernel send/recv, connections
 * route through loopback (paired sockets) or external (host adapter).
 */
function buildKernelSocketBridgeHandlers(
	dispatch: NetSocketBridgeDeps["dispatch"],
	socketTable: import("@secure-exec/core").SocketTable,
	pid: number,
): NetSocketBridgeResult {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;
	const NET_BRIDGE_TIMEOUT_SENTINEL = "__secure_exec_net_timeout__";

	// Track active kernel socket IDs for cleanup
	const activeSocketIds = new Set<number>();
	const activeServerIds = new Set<number>();
	const activeDgramIds = new Set<number>();
	// Track TLS-upgraded sockets that bypass kernel recv (host-side TLS)
	const tlsSockets = new Map<number, tls.TLSSocket>();
	const loopbackTlsTransports = new Map<string, { a: Duplex; b: Duplex }>();
	const loopbackTlsClientHello = new Map<string, SerializedTlsClientHello>();
	const pendingConnects = new Map<number, Promise<{ ok: true } | { ok: false; error: string }>>();

	type SerializedNetSocketInfo = {
		localAddress: string;
		localPort: number;
		localFamily: string;
		localPath?: string;
		remoteAddress?: string;
		remotePort?: number;
		remoteFamily?: string;
		remotePath?: string;
	};

	type SerializedNetConnectOptions = {
		host?: string;
		port?: number;
		path?: string;
	};

	type SerializedNetListenOptions = {
		host?: string;
		port?: number;
		path?: string;
		backlog?: number;
		readableAll?: boolean;
		writableAll?: boolean;
	};

	type SerializedDgramBindOptions = {
		port?: number;
		address?: string;
	};

	type SerializedDgramSendOptions = {
		data: string;
		port: number;
		address: string;
	};

	type SerializedTlsDataValue =
		| {
				kind: "buffer";
				data: string;
		  }
		| {
				kind: "string";
				data: string;
		  };

	type SerializedTlsMaterial = SerializedTlsDataValue | SerializedTlsDataValue[];

	type SerializedTlsUpgradeOptions = {
		isServer?: boolean;
		servername?: string;
		rejectUnauthorized?: boolean;
		requestCert?: boolean;
		session?: string;
		key?: SerializedTlsMaterial;
		cert?: SerializedTlsMaterial;
		ca?: SerializedTlsMaterial;
		passphrase?: string;
		ciphers?: string;
		ALPNProtocols?: string[];
		minVersion?: tls.SecureVersion;
		maxVersion?: tls.SecureVersion;
	};

	type SerializedTlsClientHello = {
		servername?: string;
		ALPNProtocols?: string[];
	};

	type SerializedTlsBridgeValue =
		| null
		| boolean
		| number
		| string
		| {
				type: "undefined";
		  }
		| {
				type: "buffer";
				data: string;
		  }
		| {
				type: "array";
				value: SerializedTlsBridgeValue[];
		  }
		| {
				type: "object";
				id: number;
				value: Record<string, SerializedTlsBridgeValue>;
		  }
		| {
				type: "ref";
				id: number;
		  };

	type KernelSocketLike = NonNullable<ReturnType<typeof socketTable.get>>;

	function addressFamily(host?: string): string {
		return host?.includes(":") ? "IPv6" : "IPv4";
	}

	function decodeTlsMaterial(
		value: SerializedTlsMaterial | undefined,
	): string | Buffer | Array<string | Buffer> | undefined {
		if (value === undefined) {
			return undefined;
		}
		const decodeOne = (entry: SerializedTlsDataValue): string | Buffer =>
			entry.kind === "buffer" ? Buffer.from(entry.data, "base64") : entry.data;
		return Array.isArray(value) ? value.map(decodeOne) : decodeOne(value);
	}

	function buildHostTlsOptions(
		options: SerializedTlsUpgradeOptions,
	): Record<string, unknown> {
		const hostOptions: Record<string, unknown> = {};
		const key = decodeTlsMaterial(options.key);
		const cert = decodeTlsMaterial(options.cert);
		const ca = decodeTlsMaterial(options.ca);
		if (key !== undefined) hostOptions.key = key;
		if (cert !== undefined) hostOptions.cert = cert;
		if (ca !== undefined) hostOptions.ca = ca;
		if (typeof options.passphrase === "string") hostOptions.passphrase = options.passphrase;
		if (typeof options.ciphers === "string") hostOptions.ciphers = options.ciphers;
		if (typeof options.session === "string") hostOptions.session = Buffer.from(options.session, "base64");
		if (Array.isArray(options.ALPNProtocols) && options.ALPNProtocols.length > 0) {
			hostOptions.ALPNProtocols = [...options.ALPNProtocols];
		}
		if (typeof options.minVersion === "string") hostOptions.minVersion = options.minVersion;
		if (typeof options.maxVersion === "string") hostOptions.maxVersion = options.maxVersion;
		if (typeof options.servername === "string") hostOptions.servername = options.servername;
		if (typeof options.requestCert === "boolean") hostOptions.requestCert = options.requestCert;
		return hostOptions;
	}

	function getLoopbackTlsKey(socketId: number, peerId: number): string {
		return socketId < peerId ? `${socketId}:${peerId}` : `${peerId}:${socketId}`;
	}

	function createTlsTransportEndpoint(
		readable: PassThrough,
		writable: PassThrough,
	): Duplex {
		const duplex = new Duplex({
			read() {
				let chunk: Buffer | null;
				while ((chunk = readable.read() as Buffer | null) !== null) {
					if (!this.push(chunk)) {
						return;
					}
				}
			},
			write(chunk, _encoding, callback) {
				if (!writable.write(chunk)) {
					writable.once("drain", callback);
					return;
				}
				callback();
			},
			final(callback) {
				writable.end();
				callback();
			},
			destroy(error, callback) {
				readable.destroy(error ?? undefined);
				writable.destroy(error ?? undefined);
				callback(error ?? null);
			},
		});

		readable.on("readable", () => {
			let chunk: Buffer | null;
			while ((chunk = readable.read() as Buffer | null) !== null) {
				if (!duplex.push(chunk)) {
					return;
				}
			}
		});
		readable.on("end", () => duplex.push(null));
		readable.on("error", (error) => duplex.destroy(error));

		return duplex;
	}

	function getLoopbackTlsTransport(socket: KernelSocketLike): Duplex {
		if (socket.peerId === undefined) {
			throw new Error(`Socket ${socket.id} has no loopback peer for TLS upgrade`);
		}
		const key = getLoopbackTlsKey(socket.id, socket.peerId);
		let pair = loopbackTlsTransports.get(key);
		if (!pair) {
			const aIn = new PassThrough();
			const bIn = new PassThrough();
			pair = {
				a: createTlsTransportEndpoint(aIn, bIn),
				b: createTlsTransportEndpoint(bIn, aIn),
			};
			loopbackTlsTransports.set(key, pair);
		}
		return socket.id < socket.peerId ? pair.a : pair.b;
	}

	function cleanupLoopbackTlsTransport(socketId: number, peerId?: number): void {
		if (peerId === undefined) {
			return;
		}
		if (tlsSockets.has(socketId) || tlsSockets.has(peerId)) {
			return;
		}
		const key = getLoopbackTlsKey(socketId, peerId);
		const pair = loopbackTlsTransports.get(key);
		if (!pair) {
			return;
		}
		pair.a.destroy();
		pair.b.destroy();
		loopbackTlsTransports.delete(key);
		loopbackTlsClientHello.delete(key);
	}

	function serializeTlsState(tlsSocket: tls.TLSSocket): string {
		let cipher: Record<string, unknown> | null = null;
			try {
				const details = tlsSocket.getCipher();
				if (details) {
					const standardName = (details as { standardName?: string }).standardName ?? details.name;
					cipher = {
						name: details.name,
						standardName,
						version: details.version,
					};
				}
		} catch {
			cipher = null;
		}
		return JSON.stringify({
			authorized: tlsSocket.authorized === true,
			authorizationError:
				typeof tlsSocket.authorizationError === "string"
					? tlsSocket.authorizationError
					: undefined,
			alpnProtocol: tlsSocket.alpnProtocol || false,
			servername: (tlsSocket as tls.TLSSocket & { servername?: string }).servername,
			protocol: tlsSocket.getProtocol?.() ?? null,
			sessionReused: tlsSocket.isSessionReused?.() === true,
			cipher,
		});
	}

	function serializeTlsBridgeValue(
		value: unknown,
		seen = new Map<object, number>(),
	): SerializedTlsBridgeValue {
		if (value === undefined) {
			return { type: "undefined" };
		}
		if (
			value === null ||
			typeof value === "boolean" ||
			typeof value === "number" ||
			typeof value === "string"
		) {
			return value;
		}
		if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
			return {
				type: "buffer",
				data: Buffer.from(value).toString("base64"),
			};
		}
		if (Array.isArray(value)) {
			return {
				type: "array",
				value: value.map((entry) => serializeTlsBridgeValue(entry, seen)),
			};
		}
		if (typeof value === "object") {
			const existingId = seen.get(value);
			if (existingId !== undefined) {
				return { type: "ref", id: existingId };
			}
			const id = seen.size + 1;
			seen.set(value, id);
			const serialized: Record<string, SerializedTlsBridgeValue> = {};
			for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
				serialized[key] = serializeTlsBridgeValue(entry, seen);
			}
			return {
				type: "object",
				id,
				value: serialized,
			};
		}
		return String(value);
	}

	function serializeTlsError(error: unknown, tlsSocket?: tls.TLSSocket): string {
		const err =
			error instanceof Error ? error : new Error(typeof error === "string" ? error : String(error));
		const payload: Record<string, unknown> = {
			message: err.message,
			name: err.name,
			stack: err.stack,
		};
		const code = (err as { code?: unknown }).code;
		if (typeof code === "string") {
			payload.code = code;
		}
		if (tlsSocket) {
			payload.authorized = tlsSocket.authorized === true;
			if (typeof tlsSocket.authorizationError === "string") {
				payload.authorizationError = tlsSocket.authorizationError;
			}
		}
		return JSON.stringify(payload);
	}

	function serializeSocketInfo(socketId: number): SerializedNetSocketInfo {
		const socket = socketTable.get(socketId);
		const localAddr = socket?.localAddr;
		const remoteAddr = socket?.remoteAddr;
		return {
			localAddress:
				localAddr && typeof localAddr === "object" && "host" in localAddr
					? localAddr.host
					: localAddr && typeof localAddr === "object" && "path" in localAddr
						? localAddr.path
						: "0.0.0.0",
			localPort:
				localAddr && typeof localAddr === "object" && "port" in localAddr
					? localAddr.port
					: 0,
			localFamily:
				localAddr && typeof localAddr === "object" && "host" in localAddr
					? addressFamily(localAddr.host)
					: localAddr && typeof localAddr === "object" && "path" in localAddr
						? "Unix"
						: "IPv4",
			...(localAddr && typeof localAddr === "object" && "path" in localAddr
				? { localPath: localAddr.path }
				: {}),
			...(remoteAddr && typeof remoteAddr === "object" && "host" in remoteAddr
				? {
						remoteAddress: remoteAddr.host,
						remotePort: remoteAddr.port,
						remoteFamily: addressFamily(remoteAddr.host),
					}
				: remoteAddr && typeof remoteAddr === "object" && "path" in remoteAddr
					? {
							remoteAddress: remoteAddr.path,
							remoteFamily: "Unix",
							remotePath: remoteAddr.path,
						}
					: {}),
		};
	}

	function getBackingSocket(socketId: number): net.Socket | undefined {
		const tlsSocket = tlsSockets.get(socketId);
		if (tlsSocket) {
			return tlsSocket;
		}
		const socket = socketTable.get(socketId);
		const hostSocket = socket?.hostSocket as { socket?: net.Socket } | undefined;
		return hostSocket?.socket;
	}

	function dispatchAsync(socketId: number, event: string, data?: string): void {
		setTimeout(() => {
			dispatch(socketId, event, data);
		}, 0);
	}

	/** Background read pump: polls kernel recv() and dispatches data/end/close. */
	function startReadPump(socketId: number): void {
		const pump = async () => {
			try {
				while (activeSocketIds.has(socketId)) {
					// Try to read data
					let data: Uint8Array | null;
					try {
						data = socketTable.recv(socketId, 65536, 0);
					} catch {
						// Socket closed or error — stop pump
						break;
					}

					if (data !== null) {
						dispatchAsync(socketId, "data", Buffer.from(data).toString("base64"));
						continue;
					}

					// No data — check if EOF
					const socket = socketTable.get(socketId);
					if (!socket) break;
					if (socket.state === "closed" || socket.state === "read-closed") {
						dispatchAsync(socketId, "end");
						break;
					}
					if (socket.peerWriteClosed || (socket.peerId === undefined && !socket.external)) {
						dispatchAsync(socketId, "end");
						break;
					}
					// For external sockets, check hostSocket EOF via readBuffer state
					if (socket.external && socket.readBuffer.length === 0 && socket.peerWriteClosed) {
						dispatchAsync(socketId, "end");
						break;
					}

					// Wait for data to arrive
					const handle = socket.readWaiters.enqueue();
					await handle.wait();
				}
			} catch {
				// Socket destroyed during pump — expected
			}
			// Dispatch close if socket was active
			if (activeSocketIds.delete(socketId)) {
				dispatchAsync(socketId, "close");
			}
		};
		pump();
	}

	// Connect — create kernel socket and start async connect + read pump
	handlers[K.netSocketConnectRaw] = (optionsJson: unknown) => {
		const options = parseJsonWithLimit<SerializedNetConnectOptions>(
			"net.socket.connect options",
			String(optionsJson),
			128 * 1024,
		);
		const isUnixPath = typeof options.path === "string" && options.path.length > 0;
		const host = String(options.host ?? "127.0.0.1");
		const port = Number(options.port ?? 0);
		const socketId = socketTable.create(
			isUnixPath ? AF_UNIX : host.includes(":") ? AF_INET6 : AF_INET,
			SOCK_STREAM,
			0,
			pid,
		);
		activeSocketIds.add(socketId);

		// Async connect completion is polled from the isolate via waitConnectRaw.
		pendingConnects.set(
			socketId,
			socketTable.connect(
				socketId,
				isUnixPath ? { path: options.path! } : { host, port },
			).then(
				() => ({ ok: true } as const),
				(error) => ({
					ok: false as const,
					error: error instanceof Error ? error.message : String(error),
				}),
			),
		);

		return socketId;
	};

	handlers[K.netSocketWaitConnectRaw] = async (socketId: unknown): Promise<string> => {
		const id = Number(socketId);
		const pending = pendingConnects.get(id);
		try {
			if (pending) {
				const result = await pending;
				if (!result.ok) {
					throw new Error(result.error);
				}
			}
			return JSON.stringify(serializeSocketInfo(id));
		} finally {
			pendingConnects.delete(id);
		}
	};

	handlers[K.netSocketReadRaw] = (socketId: unknown): string | null => {
		const id = Number(socketId);
		if (!activeSocketIds.has(id)) {
			return null;
		}
		try {
			const chunk = socketTable.recv(id, 65536, 0);
			if (chunk !== null) {
				return Buffer.from(chunk).toString("base64");
			}
			const socket = socketTable.get(id);
			if (
				!socket ||
				socket.state === "closed" ||
				socket.state === "read-closed" ||
				socket.peerWriteClosed
			) {
				return null;
			}
			return NET_BRIDGE_TIMEOUT_SENTINEL;
		} catch (error) {
			if (error instanceof Error && error.message.includes("EAGAIN")) {
				return NET_BRIDGE_TIMEOUT_SENTINEL;
			}
			return null;
		}
	};

	handlers[K.netSocketSetNoDelayRaw] = (socketId: unknown, enable: unknown) => {
		const id = Number(socketId);
		socketTable.setsockopt(id, IPPROTO_TCP, TCP_NODELAY, enable ? 1 : 0);
		getBackingSocket(id)?.setNoDelay(Boolean(enable));
	};

	handlers[K.netSocketSetKeepAliveRaw] = (
		socketId: unknown,
		enable: unknown,
		initialDelaySeconds: unknown,
	) => {
		const id = Number(socketId);
		const delaySeconds = Math.max(0, Number(initialDelaySeconds) || 0);
		socketTable.setsockopt(id, SOL_SOCKET, SO_KEEPALIVE, enable ? 1 : 0);
		getBackingSocket(id)?.setKeepAlive(Boolean(enable), delaySeconds * 1000);
	};

	// Write — send data through kernel socket
	handlers[K.netSocketWriteRaw] = (
		socketId: unknown,
		dataBase64: unknown,
	) => {
		const id = Number(socketId);
		// TLS-upgraded sockets write directly to host TLS socket
		const tlsSocket = tlsSockets.get(id);
		if (tlsSocket) {
			tlsSocket.write(Buffer.from(String(dataBase64), "base64"));
			return;
		}
		const data = Buffer.from(String(dataBase64), "base64");
		socketTable.send(id, new Uint8Array(data), 0);
	};

	// End — half-close write side
	handlers[K.netSocketEndRaw] = (socketId: unknown) => {
		const id = Number(socketId);
		const tlsSocket = tlsSockets.get(id);
		if (tlsSocket) {
			tlsSocket.end();
			return;
		}
		try {
			socketTable.shutdown(id, "write");
		} catch {
			// Socket may already be closed
		}
	};

	// Destroy — close kernel socket
	handlers[K.netSocketDestroyRaw] = (socketId: unknown) => {
		const id = Number(socketId);
		const socket = socketTable.get(id);
		const tlsSocket = tlsSockets.get(id);
		if (tlsSocket) {
			tlsSocket.destroy();
			tlsSockets.delete(id);
		}
		cleanupLoopbackTlsTransport(id, socket?.peerId);
		socketTable.get(id)?.readWaiters.wakeAll();
		if (activeSocketIds.has(id)) {
			activeSocketIds.delete(id);
			try {
				socketTable.close(id, pid);
			} catch {
				// Already closed
			}
		}
	};

	// TLS upgrade — for external kernel sockets, unwrap the host socket
	// and wrap with TLS. Loopback sockets cannot be TLS-upgraded (no real TCP).
	handlers[K.netSocketUpgradeTlsRaw] = (
		socketId: unknown,
		optionsJson: unknown,
	) => {
		const id = Number(socketId);
		const socket = socketTable.get(id);
		if (!socket) throw new Error(`Socket ${id} not found for TLS upgrade`);

		const options = optionsJson
			? parseJsonWithLimit<SerializedTlsUpgradeOptions>(
					"net.socket.upgradeTls options",
					String(optionsJson),
					256 * 1024,
				)
			: {};
		const hostTlsOptions = buildHostTlsOptions(options);
		const peerId = socket.peerId;
		const loopbackTlsKey = peerId === undefined ? undefined : getLoopbackTlsKey(id, peerId);

		if (!options.isServer && loopbackTlsKey) {
			loopbackTlsClientHello.set(loopbackTlsKey, {
				servername: options.servername,
				ALPNProtocols: options.ALPNProtocols,
			});
		}

		let transport: net.Socket | Duplex;
		if (socket.external && socket.hostSocket) {
			const hostSocket = socket.hostSocket as unknown as { socket?: net.Socket };
			const realSocket = hostSocket.socket;
			if (!realSocket) {
				throw new Error(`Socket ${id} has no underlying TCP socket for TLS upgrade`);
			}
			socket.hostSocket = undefined;
			transport = realSocket;
		} else {
			transport = getLoopbackTlsTransport(socket);
		}

		const tlsSocket = options.isServer
			? new tls.TLSSocket(transport, {
					isServer: true,
					secureContext: tls.createSecureContext(hostTlsOptions),
					requestCert: options.requestCert === true,
					rejectUnauthorized: options.rejectUnauthorized === true,
				})
			: tls.connect({
					socket: transport,
					...hostTlsOptions,
					rejectUnauthorized: options.rejectUnauthorized !== false,
				});

		// Track TLS socket for write/end/destroy bypass
		tlsSockets.set(id, tlsSocket);

		tlsSocket.on("secureConnect", () =>
			dispatchAsync(id, "secureConnect", serializeTlsState(tlsSocket)),
		);
		tlsSocket.on("secure", () =>
			dispatchAsync(id, "secure", serializeTlsState(tlsSocket)),
		);
		tlsSocket.on("session", (session: Buffer) =>
			dispatchAsync(id, "session", session.toString("base64")),
		);
		tlsSocket.on("data", (chunk: Buffer) =>
			dispatchAsync(id, "data", chunk.toString("base64")),
		);
		tlsSocket.on("end", () => dispatchAsync(id, "end"));
		tlsSocket.on("error", (err: Error) =>
			dispatchAsync(id, "error", serializeTlsError(err, tlsSocket)),
		);
		tlsSocket.on("close", () => {
			tlsSockets.delete(id);
			activeSocketIds.delete(id);
			cleanupLoopbackTlsTransport(id, peerId);
			dispatchAsync(id, "close");
		});
	};

	handlers[K.netSocketGetTlsClientHelloRaw] = (socketId: unknown): string => {
		const id = Number(socketId);
		const socket = socketTable.get(id);
		if (!socket || socket.peerId === undefined) {
			return "{}";
		}
		const entry = loopbackTlsClientHello.get(getLoopbackTlsKey(id, socket.peerId));
		return JSON.stringify(entry ?? {});
	};

	handlers[K.netSocketTlsQueryRaw] = (
		socketId: unknown,
		query: unknown,
		detailed?: unknown,
	): string => {
		const tlsSocket = tlsSockets.get(Number(socketId)) as tls.TLSSocket | undefined;
		if (!tlsSocket) {
			return JSON.stringify({ type: "undefined" });
		}
		let result: unknown;
		switch (String(query)) {
			case "getSession":
				result = tlsSocket.getSession();
				break;
			case "isSessionReused":
				result = tlsSocket.isSessionReused();
				break;
			case "getPeerCertificate":
				result = tlsSocket.getPeerCertificate(Boolean(detailed));
				break;
			case "getCertificate":
				result = tlsSocket.getCertificate();
				break;
			case "getProtocol":
				result = tlsSocket.getProtocol();
				break;
			case "getCipher":
				result = tlsSocket.getCipher();
				break;
			default:
				result = undefined;
				break;
		}
		return JSON.stringify(serializeTlsBridgeValue(result));
	};

	handlers[K.tlsGetCiphersRaw] = (): string => JSON.stringify(tls.getCiphers());

	handlers[K.netServerListenRaw] = async (optionsJson: unknown): Promise<string> => {
		const options = parseJsonWithLimit<SerializedNetListenOptions>(
			"net.server.listen options",
			String(optionsJson),
			128 * 1024,
		);
		const isUnixPath = typeof options.path === "string" && options.path.length > 0;
		const host = String(options.host ?? "127.0.0.1");
		const serverId = socketTable.create(
			isUnixPath ? AF_UNIX : host.includes(":") ? AF_INET6 : AF_INET,
			SOCK_STREAM,
			0,
			pid,
		);
		activeServerIds.add(serverId);
		const socketMode =
			options.readableAll || options.writableAll
				? 0o600 |
					(options.readableAll ? 0o044 : 0) |
					(options.writableAll ? 0o022 : 0)
				: undefined;
		await socketTable.bind(
			serverId,
			isUnixPath
				? { path: options.path! }
				: {
						host,
						port: Number(options.port ?? 0),
					},
			socketMode === undefined ? undefined : { mode: socketMode },
		);
		await socketTable.listen(serverId, Number(options.backlog ?? 511));
		return JSON.stringify({
			serverId,
			address: serializeSocketInfo(serverId),
		});
	};

	handlers[K.netServerAcceptRaw] = (serverId: unknown): string | null => {
		const id = Number(serverId);
		if (!activeServerIds.has(id)) {
			return null;
		}
		const listener = socketTable.get(id);
		if (!listener || listener.state !== "listening") {
			return null;
		}
		const acceptedId = socketTable.accept(id);
		if (acceptedId === null) {
			return NET_BRIDGE_TIMEOUT_SENTINEL;
		}
		activeSocketIds.add(acceptedId);
		return JSON.stringify({
			socketId: acceptedId,
			info: serializeSocketInfo(acceptedId),
		});
	};

	handlers[K.netServerCloseRaw] = async (serverId: unknown): Promise<void> => {
		const id = Number(serverId);
		activeServerIds.delete(id);
		socketTable.get(id)?.acceptWaiters.wakeAll();
		try {
			socketTable.close(id, pid);
		} catch {
			// Already closed
		}
	};

	handlers[K.dgramSocketCreateRaw] = (type: unknown): number => {
		const socketType = String(type);
		const domain = socketType === "udp6" ? AF_INET6 : AF_INET;
		const socketId = socketTable.create(domain, SOCK_DGRAM, 0, pid);
		activeDgramIds.add(socketId);
		return socketId;
	};

	handlers[K.dgramSocketBindRaw] = async (
		socketId: unknown,
		optionsJson: unknown,
	): Promise<string> => {
		const id = Number(socketId);
		const socket = socketTable.get(id);
		if (!socket) {
			throw new Error(`UDP socket ${id} not found`);
		}
		const options = parseJsonWithLimit<SerializedDgramBindOptions>(
			"dgram.socket.bind options",
			String(optionsJson),
			128 * 1024,
		);
		const host = String(
			options.address ??
				(socket.domain === AF_INET6 ? "::" : "0.0.0.0"),
		);
		await socketTable.bind(id, {
			host,
			port: Number(options.port ?? 0),
		});
		return JSON.stringify(serializeSocketInfo(id));
	};

	handlers[K.dgramSocketRecvRaw] = (socketId: unknown): string | null => {
		const id = Number(socketId);
		if (!activeDgramIds.has(id)) {
			return null;
		}
		try {
			const socket = socketTable.get(id);
			if (!socket || socket.state === "closed") {
				return null;
			}
			const message = socketTable.recvFrom(id, 65535, 0);
			if (message === null) {
				return NET_BRIDGE_TIMEOUT_SENTINEL;
			}
			return JSON.stringify({
				data: Buffer.from(message.data).toString("base64"),
				rinfo:
					"path" in message.srcAddr
						? {
								address: message.srcAddr.path,
								family: "unix",
								port: 0,
								size: message.data.length,
							}
						: {
								address: message.srcAddr.host,
								family: addressFamily(message.srcAddr.host),
								port: message.srcAddr.port,
								size: message.data.length,
							},
			});
		} catch (error) {
			if (error instanceof Error && error.message.includes("EAGAIN")) {
				return NET_BRIDGE_TIMEOUT_SENTINEL;
			}
			return null;
		}
	};

	handlers[K.dgramSocketSendRaw] = async (
		socketId: unknown,
		optionsJson: unknown,
	): Promise<number> => {
		const id = Number(socketId);
		const options = parseJsonWithLimit<SerializedDgramSendOptions>(
			"dgram.socket.send options",
			String(optionsJson),
			256 * 1024,
		);
		const data = Buffer.from(options.data, "base64");
		return socketTable.sendTo(
			id,
			new Uint8Array(data),
			0,
			{ host: String(options.address), port: Number(options.port) },
		);
	};

	handlers[K.dgramSocketCloseRaw] = async (socketId: unknown): Promise<void> => {
		const id = Number(socketId);
		activeDgramIds.delete(id);
		socketTable.get(id)?.readWaiters.wakeAll();
		try {
			socketTable.close(id, pid);
		} catch {
			// Already closed
		}
	};

	handlers[K.dgramSocketAddressRaw] = (socketId: unknown): string => {
		const id = Number(socketId);
		const socket = socketTable.get(id);
		if (!socket?.localAddr || "path" in socket.localAddr) {
			throw new Error("getsockname EBADF");
		}
		return JSON.stringify({
			address: socket.localAddr.host,
			family: addressFamily(socket.localAddr.host),
			port: socket.localAddr.port,
		});
	};

	handlers[K.dgramSocketSetBufferSizeRaw] = (
		socketId: unknown,
		which: unknown,
		size: unknown,
	): void => {
		const optname = which === "send" ? SO_SNDBUF : SO_RCVBUF;
		socketTable.setsockopt(Number(socketId), SOL_SOCKET, optname, Number(size));
	};

	handlers[K.dgramSocketGetBufferSizeRaw] = (
		socketId: unknown,
		which: unknown,
	): number => {
		const optname = which === "send" ? SO_SNDBUF : SO_RCVBUF;
		return socketTable.getsockopt(Number(socketId), SOL_SOCKET, optname) ?? 0;
	};

	const dispose = () => {
		for (const id of activeServerIds) {
			try { socketTable.close(id, pid); } catch { /* best effort */ }
		}
		activeServerIds.clear();
		for (const id of activeDgramIds) {
			try { socketTable.close(id, pid); } catch { /* best effort */ }
		}
		activeDgramIds.clear();
		for (const id of activeSocketIds) {
			try { socketTable.close(id, pid); } catch { /* best effort */ }
		}
		activeSocketIds.clear();
		for (const socket of tlsSockets.values()) {
			socket.destroy();
		}
		tlsSockets.clear();
		for (const pair of loopbackTlsTransports.values()) {
			pair.a.destroy();
			pair.b.destroy();
		}
		loopbackTlsTransports.clear();
		loopbackTlsClientHello.clear();
	};

	return { handlers, dispose };
}

/** Dependencies for building sync module resolution bridge handlers. */
export interface ModuleResolutionBridgeDeps {
	/** Translate sandbox path (e.g. /root/node_modules/...) to host path. */
	sandboxToHostPath: (sandboxPath: string) => string | null;
	/** Translate host path back to sandbox path. */
	hostToSandboxPath: (hostPath: string) => string;
	/** Additional host directories used as require.resolve paths for transitive deps. */
	additionalResolvePaths?: string[];
}

function normalizeModuleResolveContext(referrer: string): string {
	if (!referrer || referrer.endsWith("/")) {
		return referrer || "/";
	}

	return pathDirname(referrer) !== referrer && /\.[^/]+$/.test(referrer)
		? pathDirname(referrer)
		: referrer;
}

function selectPackageExportTarget(
	entry: unknown,
	mode: "require" | "import",
): string | null {
	if (typeof entry === "string") {
		return entry;
	}

	if (Array.isArray(entry)) {
		for (const candidate of entry) {
			const resolved = selectPackageExportTarget(candidate, mode);
			if (resolved) {
				return resolved;
			}
		}
		return null;
	}

	if (!entry || typeof entry !== "object") {
		return null;
	}

	const conditionalEntry = entry as {
		default?: unknown;
		import?: unknown;
		require?: unknown;
	};
	const candidates =
		mode === "import"
			? [conditionalEntry.import, conditionalEntry.default, conditionalEntry.require]
			: [conditionalEntry.require, conditionalEntry.default, conditionalEntry.import];

	for (const candidate of candidates) {
		const resolved = selectPackageExportTarget(candidate, mode);
		if (resolved) {
			return resolved;
		}
	}

	return null;
}

/**
 * Resolve a `#`-prefixed subpath import by walking up directories to find
 * the nearest package.json with an `imports` field. Uses synchronous I/O.
 */
function resolvePackageImportSync(
	specifier: string,
	startDir: string,
	mode: "require" | "import" = "require",
): string | null {
	let cur = startDir;
	while (cur !== pathDirname(cur)) {
		const pkgJsonPath = pathJoin(cur, "package.json");
		if (existsSync(pkgJsonPath)) {
			try {
				const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf-8"));
				if (pkg.imports && typeof pkg.imports === "object") {
					const entry = pkg.imports[specifier];
					if (typeof entry === "string") {
						return pathJoin(cur, entry);
					}
					if (entry && typeof entry === "object") {
						const target = selectPackageExportTarget(entry, mode);
						if (target) return pathJoin(cur, target);
					}
				}
			} catch { /* malformed package.json, try parent */ }
		}
		cur = pathDirname(cur);
	}
	return null;
}

/**
 * Resolve a package specifier by walking up directories and reading package.json exports.
 * Handles both root imports ('pkg') and subpath imports ('pkg/sub').
 */
function resolvePackageExport(
	req: string,
	startDir: string,
	mode: "require" | "import" = "require",
): string | null {
	// Split into package name and subpath
	const parts = req.startsWith("@") ? req.split("/") : [req.split("/")[0], ...req.split("/").slice(1)];
	const pkgName = req.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
	const subpath = req.startsWith("@")
		? (parts.length > 2 ? "./" + parts.slice(2).join("/") : ".")
		: (parts.length > 1 ? "./" + parts.slice(1).join("/") : ".");

	let cur = startDir;
	while (cur !== pathDirname(cur)) {
		const pkgJsonPath = pathJoin(cur, "node_modules", ...pkgName.split("/"), "package.json");
		if (existsSync(pkgJsonPath)) {
			const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf-8"));
			let entry: string | undefined;
			if (pkg.exports) {
				const exportEntry =
					subpath === "." &&
					typeof pkg.exports === "object" &&
					pkg.exports !== null &&
					!Array.isArray(pkg.exports) &&
					!("." in (pkg.exports as Record<string, unknown>))
						? pkg.exports
						: (pkg.exports as Record<string, unknown>)[subpath];
				const resolvedEntry = selectPackageExportTarget(exportEntry, mode);
				if (resolvedEntry) entry = resolvedEntry;
			}
			if (!entry && subpath === ".") entry = pkg.main;
			if (entry) return pathResolve(pathDirname(pkgJsonPath), entry);
		}
		cur = pathDirname(cur);
	}
	return null;
}

const hostRequire = createRequire(import.meta.url);

/**
 * Build sync module resolution bridge handlers.
 *
 * These use Node.js require.resolve() and readFileSync() directly,
 * avoiding the async VirtualFileSystem path. Needed because the async
 * applySyncPromise pattern can't nest inside synchronous bridge
 * callbacks (e.g. net socket data events that trigger require()).
 */
export function buildModuleResolutionBridgeHandlers(
	deps: ModuleResolutionBridgeDeps,
): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;

	// Sync require.resolve — translates sandbox paths and uses Node.js resolution.
	// Falls back to realpath + manual package.json resolution for pnpm/ESM packages.
	handlers[K.resolveModuleSync] = (
		request: unknown,
		fromDir: unknown,
		requestedMode?: unknown,
	) => {
		const req = String(request);
		const resolveMode =
			requestedMode === "require" || requestedMode === "import"
				? requestedMode
				: "require";

		// Builtins don't need filesystem resolution
		const builtin = normalizeBuiltinSpecifier(req);
		if (builtin) return builtin;

		// Translate sandbox fromDir to host path for resolution context
		const referrer = String(fromDir);
		const sandboxDir = normalizeModuleResolveContext(referrer);
		const hostDir = normalizeModuleResolveContext(
			deps.sandboxToHostPath(referrer) ??
				deps.sandboxToHostPath(sandboxDir) ??
				sandboxDir,
		);

		// Handle absolute path specifiers directly
		if (req.startsWith("/")) {
			return req;
		}

		// Handle #-prefixed subpath imports (package.json "imports" field)
		if (req.startsWith("#")) {
			// Try host-side resolution first with the translated hostDir.
			let resolved = resolvePackageImportSync(req, hostDir, resolveMode);
			if (!resolved) {
				// Fallback: try resolving from the realpath of hostDir.
				// pnpm symlinks can prevent walk-up from finding the owning
				// package.json when the hostDir is a symlink.
				try {
					const realHostDir = realpathSync(hostDir);
					if (realHostDir !== hostDir) {
						resolved = resolvePackageImportSync(req, realHostDir, resolveMode);
					}
				} catch { /* realpath failed, skip */ }
			}
			if (!resolved) {
				// hostDir translation may have failed (e.g. transitive deps not
				// in packageRoots). Try using require.resolve with additional
				// resolve paths to find the owning package on the host.
				const nmPrefix = "/root/node_modules/";
				if (sandboxDir.startsWith(nmPrefix)) {
					const rest = sandboxDir.slice(nmPrefix.length);
					const parts = rest.split("/");
					const pkgName = parts[0].startsWith("@")
						? parts.slice(0, 2).join("/")
						: parts[0];
					const paths = deps.additionalResolvePaths ?? [];
					for (const searchPath of paths) {
						try {
							const pkgJsonPath = hostRequire.resolve(
								`${pkgName}/package.json`,
								{ paths: [searchPath] },
							);
							const pkgDir = pathDirname(pkgJsonPath);
							resolved = resolvePackageImportSync(req, pkgDir, resolveMode);
							if (resolved) break;
						} catch { /* not found in this path */ }
					}
				}
			}
			return resolved ? deps.hostToSandboxPath(resolved) : null;
		}

		const resolveFromExports = (dir: string) => {
			const resolved = resolvePackageExport(req, dir, resolveMode);
			return resolved ? deps.hostToSandboxPath(resolved) : null;
		};

		if (resolveMode === "import") {
			const resolved = resolveFromExports(hostDir);
			if (resolved) return resolved;
		}

		// Try require.resolve first
		try {
			const resolved = hostRequire.resolve(req, { paths: [hostDir] });
			return deps.hostToSandboxPath(resolved);
		} catch { /* CJS resolution failed */ }

		// Fallback: follow symlinks and try ESM-compatible resolution
		try {
			let realDir: string;
			try { realDir = realpathSync(hostDir); } catch { realDir = hostDir; }
			if (resolveMode === "import") {
				const resolved = resolveFromExports(realDir);
				if (resolved) return resolved;
			}
			// Try require.resolve from real path
			try {
				const resolved = hostRequire.resolve(req, { paths: [realDir] });
				return deps.hostToSandboxPath(resolved);
			} catch { /* ESM-only, manual resolution */ }
			// Manual package.json resolution for ESM packages
			const resolved = resolveFromExports(realDir);
			if (resolved) return resolved;
		} catch { /* fallback failed */ }

		// Last resort: try resolving from the host process itself (no path restrictions).
		// This handles Node.js-bundled packages like undici that aren't in node: namespace.
		try {
			const resolved = hostRequire.resolve(req);
			return deps.hostToSandboxPath(resolved);
		} catch { /* truly not found */ }
		return null;
	};

	// Sync file read — translates sandbox path and applies parser-backed
	// CJS transforms when require() needs ESM or import() support.
	handlers[K.loadFileSync] = (filePath: unknown) => {
		const sandboxPath = String(filePath);
		if (sandboxPath.includes("balanced")) console.error(`[loadFileSync] path=${sandboxPath}`);
		const hostPath = deps.sandboxToHostPath(sandboxPath) ?? sandboxPath;
		return loadHostModuleSourceSync(hostPath, sandboxPath, "require");
	};

	return handlers;
}

// Env vars that could hijack child processes (library injection, node flags)
const DANGEROUS_ENV_KEYS = new Set([
	"LD_PRELOAD",
	"LD_LIBRARY_PATH",
	"NODE_OPTIONS",
	"DYLD_INSERT_LIBRARIES",
]);

/** Strip env vars that allow library injection or node flag smuggling. */
export function stripDangerousEnv(
	env: Record<string, string> | undefined,
): Record<string, string> | undefined {
	if (!env) return env;
	const result: Record<string, string> = {};
	for (const [key, value] of Object.entries(env)) {
		if (!DANGEROUS_ENV_KEYS.has(key)) {
			result[key] = value;
		}
	}
	return result;
}

export function emitConsoleEvent(
	onStdio: StdioHook | undefined,
	event: StdioEvent,
): void {
	if (!onStdio) return;
	try {
		onStdio(event);
	} catch {
		// Keep runtime execution deterministic even when host hooks fail.
	}
}

/** Dependencies for console bridge handlers. */
export interface ConsoleBridgeDeps {
	onStdio?: StdioHook;
	budgetState: BudgetState;
	maxOutputBytes?: number;
}

/** Build console/logging bridge handlers. */
export function buildConsoleBridgeHandlers(deps: ConsoleBridgeDeps): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;

	handlers[K.log] = (msg: unknown) => {
		const str = String(msg);
		if (deps.maxOutputBytes !== undefined) {
			const bytes = Buffer.byteLength(str, "utf8");
			if (deps.budgetState.outputBytes + bytes > deps.maxOutputBytes) return;
			deps.budgetState.outputBytes += bytes;
		}
		emitConsoleEvent(deps.onStdio, { channel: "stdout", message: str });
	};

	handlers[K.error] = (msg: unknown) => {
		const str = String(msg);
		if (deps.maxOutputBytes !== undefined) {
			const bytes = Buffer.byteLength(str, "utf8");
			if (deps.budgetState.outputBytes + bytes > deps.maxOutputBytes) return;
			deps.budgetState.outputBytes += bytes;
		}
		emitConsoleEvent(deps.onStdio, { channel: "stderr", message: str });
	};

	return handlers;
}

/** Dependencies for module loading bridge handlers. */
export interface ModuleLoadingBridgeDeps {
	filesystem: VirtualFileSystem;
	resolutionCache: ResolutionCache;
	resolveMode?: "require" | "import";
	/** Convert sandbox path to host path for pnpm/symlink resolution fallback. */
	sandboxToHostPath?: (sandboxPath: string) => string | null;
}

function getStaticBuiltinRequireSource(moduleName: string): string | null {
	switch (moduleName) {
		case "fs":
			return "module.exports = globalThis.bridge?.fs || globalThis.bridge?.default || {};";
		case "fs/promises":
			return "module.exports = (globalThis.bridge?.fs || globalThis.bridge?.default || {}).promises || {};";
		case "module":
			return `module.exports = ${
				"globalThis.bridge?.module || {" +
				"createRequire: globalThis._createRequire || function(f) {" +
				"const dir = f.replace(/\\\\[^\\\\]*$/, '') || '/';" +
				"return function(m) { return globalThis._requireFrom(m, dir); };" +
				"}," +
				"Module: { builtinModules: [] }," +
				"isBuiltin: () => false," +
				"builtinModules: []" +
				"}"
			};`;
		case "os":
			return "module.exports = globalThis._osModule || {};";
		case "http":
			return "module.exports = globalThis._httpModule || globalThis.bridge?.network?.http || {};";
		case "https":
			return "module.exports = globalThis._httpsModule || globalThis.bridge?.network?.https || {};";
		case "http2":
			return "module.exports = globalThis._http2Module || {};";
		case "dns":
			return "module.exports = globalThis._dnsModule || globalThis.bridge?.network?.dns || {};";
		case "child_process":
			return "module.exports = globalThis._childProcessModule || globalThis.bridge?.childProcess || {};";
		case "process":
			return "module.exports = globalThis.process || {};";
		case "v8":
			return "module.exports = globalThis._moduleCache?.v8 || {};";
		default:
			return null;
	}
}

function loadHostModuleSourceSync(
	readPath: string,
	logicalPath: string,
	loadMode: "require" | "import",
): string | null {
	try {
		const source = readFileSync(readPath, "utf-8");
		return loadMode === "require"
			? transformSourceForRequireSync(source, logicalPath)
			: transformSourceForImportSync(source, logicalPath, readPath);
	} catch {
		return null;
	}
}

/** Build module loading bridge handlers (loadPolyfill, resolveModule, loadFile). */
export function buildModuleLoadingBridgeHandlers(
	deps: ModuleLoadingBridgeDeps,
	/** Extra handlers to dispatch through _loadPolyfill for V8 runtime compatibility. */
	dispatchHandlers?: BridgeHandlers,
): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;

	// Polyfill loading — also serves as bridge dispatch multiplexer.
	// The V8 runtime binary only registers a fixed set of bridge globals.
	// Newer handlers (crypto, net sockets, etc.) are dispatched through
	// _loadPolyfill with a "__bd:" prefix.
	handlers[K.loadPolyfill] = async (moduleName: unknown): Promise<string | null> => {
		const nameStr = String(moduleName);

		// Bridge dispatch: "__bd:methodName:base64args"
		if (nameStr.startsWith("__bd:") && dispatchHandlers) {
			const colonIdx = nameStr.indexOf(":", 5);
			const method = nameStr.substring(5, colonIdx > 0 ? colonIdx : undefined);
			const argsJson = colonIdx > 0 ? nameStr.substring(colonIdx + 1) : "[]";
			const handler = dispatchHandlers[method];
			if (!handler) return JSON.stringify({ __bd_error: `No handler: ${method}` });
			try {
				const args = restoreDispatchArgument(JSON.parse(argsJson));
				const result = await handler(...(Array.isArray(args) ? args : [args]));
				return JSON.stringify({ __bd_result: result });
			} catch (err) {
				return JSON.stringify({ __bd_error: serializeDispatchError(err) });
			}
		}

		const name = nameStr.replace(/^node:/, "");
		if (name === "fs" || name === "child_process" || name === "http" ||
			name === "https" || name === "http2" || name === "dns" ||
			name === "os" || name === "module") {
			return null;
		}
		if (!hasPolyfill(name)) return null;
		let code = polyfillCodeCache.get(name);
		if (!code) {
			code = await bundlePolyfill(name);
			polyfillCodeCache.set(name, code);
		}
		return code;
	};

	// Async module path resolution via VFS
	// V8 ESM module resolve sends the full file path as referrer, not a directory.
	// Extract dirname when the referrer looks like a file path.
	// Falls back to Node.js require.resolve() with realpath for pnpm compatibility.
	handlers[K.resolveModule] = async (
		request: unknown,
		fromDir: unknown,
		requestedMode?: unknown,
	): Promise<string | null> => {
		const req = String(request);
		const resolveMode =
			requestedMode === "require" || requestedMode === "import"
				? requestedMode
				: (deps.resolveMode ?? "require");
		const builtin = normalizeBuiltinSpecifier(req);
		if (builtin) return builtin;
		let dir = String(fromDir);
		if (/\.[cm]?[jt]sx?$/.test(dir)) {
			const lastSlash = dir.lastIndexOf("/");
			if (lastSlash > 0) dir = dir.slice(0, lastSlash);
		}
		// Handle absolute path specifiers directly (e.g. from V8 module linker)
		if (req.startsWith("/")) {
			return req;
		}
		const vfsResult = await resolveModule(
			req,
			dir,
			deps.filesystem,
			resolveMode,
			deps.resolutionCache,
		);
		if (vfsResult) return vfsResult;
		// Fallback: resolve through real host paths for pnpm symlink compatibility.
		const hostDir = deps.sandboxToHostPath?.(dir) ?? dir;
		try {
			let realDir: string;
			try { realDir = realpathSync(hostDir); } catch { realDir = hostDir; }
			if (resolveMode === "import") {
				const resolvedImport = resolvePackageExport(req, realDir, "import");
				if (resolvedImport) return resolvedImport;
			}
			// Try require.resolve (works for CJS packages)
			try {
				return hostRequire.resolve(req, { paths: [realDir] });
			} catch { /* ESM-only, try manual resolution */ }
			// Manual package.json resolution for ESM packages
			const resolved = resolvePackageExport(req, realDir, resolveMode);
			if (resolved) return resolved;
		} catch { /* resolution failed */ }
		return null;
	};

	// Dynamic import bridge — returns null to fall back to require() in the sandbox.
	// V8 ESM module mode handles static imports natively via module_resolve_callback;
	// this handler covers the __dynamicImport() path used in exec mode.
	handlers[K.dynamicImport] = async (): Promise<null> => null;

	// Async file read for CommonJS and ESM loader paths.
	// Also serves ESM wrappers for built-in modules (fs, path, etc.) when
	// used from V8's ES module system which calls _loadFile after _resolveModule.
	handlers[K.loadFile] = (
		path: unknown,
		requestedMode?: unknown,
	): string | null | Promise<string | null> => {
		const p = String(path);
		const loadMode =
			requestedMode === "require" || requestedMode === "import"
				? requestedMode
				: (deps.resolveMode ?? "require");
		// Built-in module ESM wrappers (V8 module system resolves 'fs' then loads it)
		const bare = p.replace(/^node:/, "");
		if (loadMode === "require") {
			const builtinRequireSource = getStaticBuiltinRequireSource(bare);
			if (builtinRequireSource) return builtinRequireSource;
		}
		const builtinBindingExpression = getBuiltinBindingExpression(bare);
		if (builtinBindingExpression) {
			return createBuiltinESMWrapper(
				builtinBindingExpression,
				getHostBuiltinNamedExports(bare),
			);
		}
		const builtin = getStaticBuiltinWrapperSource(bare);
		if (builtin) return builtin;
		// Polyfill-backed builtins (crypto, zlib, etc.)
		if (hasPolyfill(bare)) {
			return createBuiltinESMWrapper(
				`globalThis._requireFrom(${JSON.stringify(bare)}, "/")`,
				getHostBuiltinNamedExports(bare),
			);
		}

		// Fallback for Node.js builtin submodules (e.g. stream/consumers, path/posix).
		// These use the node: prefix or match known builtin patterns.
		// Delegate to the CJS require stub which handles more modules than
		// the static ESM wrappers above.
		if (p.startsWith("node:") && !bare.includes(".")) {
			return createBuiltinESMWrapper(
				`globalThis._requireFrom(${JSON.stringify(bare)}, "/")`,
				getHostBuiltinNamedExports(bare),
			);
		}

		const hostPath = deps.sandboxToHostPath?.(p) ?? p;
		const syncSource = loadHostModuleSourceSync(hostPath, p, loadMode);
		if (syncSource !== null) {
			return syncSource;
		}

		// Regular files load differently for CommonJS require() vs V8's ESM loader.
		return (async () => {
			const source = await loadFile(p, deps.filesystem);
			if (source === null) return null;
			if (loadMode === "require") {
				return transformSourceForRequire(source, p);
			}
			return transformSourceForImport(source, p);
		})();
	};

	return handlers;
}

/** Dependencies for timer bridge handlers. */
export interface TimerBridgeDeps {
	budgetState: BudgetState;
	maxBridgeCalls?: number;
	activeHostTimers: Set<ReturnType<typeof setTimeout>>;
}

/** Build timer bridge handler. */
export function buildTimerBridgeHandlers(deps: TimerBridgeDeps): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;

	handlers[K.scheduleTimer] = (delayMs: unknown) => {
		checkBridgeBudget(deps);
		return new Promise<void>((resolve) => {
			const id = globalThis.setTimeout(() => {
				deps.activeHostTimers.delete(id);
				resolve();
			}, Number(delayMs));
			deps.activeHostTimers.add(id);
		});
	};

	return handlers;
}

function serializeMimeTypeState(value: InstanceType<typeof hostUtil.MIMEType>) {
	return {
		value: String(value),
		essence: value.essence,
		type: value.type,
		subtype: value.subtype,
		params: Array.from(value.params.entries()),
	};
}

export function buildMimeBridgeHandlers(): BridgeHandlers {
	return {
		mimeBridge: (operation: unknown, input: unknown, ...args: unknown[]) => {
			const mime = new hostUtil.MIMEType(String(input));
			switch (String(operation)) {
				case "parse":
					return serializeMimeTypeState(mime);
				case "setType":
					mime.type = String(args[0]);
					return serializeMimeTypeState(mime);
				case "setSubtype":
					mime.subtype = String(args[0]);
					return serializeMimeTypeState(mime);
				case "setParam":
					mime.params.set(String(args[0]), String(args[1]));
					return serializeMimeTypeState(mime);
				case "deleteParam":
					mime.params.delete(String(args[0]));
					return serializeMimeTypeState(mime);
				default:
					throw new Error(`Unsupported MIME bridge operation: ${String(operation)}`);
			}
		},
	};
}

export interface KernelTimerDispatchDeps {
	timerTable: import("@secure-exec/core").TimerTable;
	pid: number;
	budgetState: BudgetState;
	maxBridgeCalls?: number;
	activeHostTimers: Set<ReturnType<typeof setTimeout>>;
	sendStreamEvent(eventType: string, payload: Uint8Array): void;
}

export function buildKernelTimerDispatchHandlers(
	deps: KernelTimerDispatchDeps,
): BridgeHandlers {
	const handlers: BridgeHandlers = {};

	handlers.kernelTimerCreate = (delayMs: unknown, repeat: unknown) => {
		checkBridgeBudget(deps);
		const normalizedDelay = Number(delayMs);
		return deps.timerTable.createTimer(
			deps.pid,
			Number.isFinite(normalizedDelay) && normalizedDelay > 0
				? Math.floor(normalizedDelay)
				: 0,
			Boolean(repeat),
			() => {},
		);
	};

	handlers.kernelTimerArm = (timerId: unknown) => {
		checkBridgeBudget(deps);
		const timer = deps.timerTable.get(Number(timerId));
		if (!timer || timer.pid !== deps.pid || timer.cleared) {
			return;
		}

		const dispatchFire = () => {
			const activeTimer = deps.timerTable.get(timer.id);
			if (!activeTimer || activeTimer.pid !== deps.pid || activeTimer.cleared) {
				return;
			}

			activeTimer.hostHandle = undefined;
			if (!activeTimer.repeat) {
				deps.timerTable.clearTimer(activeTimer.id, deps.pid);
			}
			deps.sendStreamEvent(
				"timer",
				Buffer.from(JSON.stringify({ timerId: activeTimer.id })),
			);
		};

		if (timer.delayMs <= 0) {
			queueMicrotask(dispatchFire);
			return;
		}

		const hostHandle = globalThis.setTimeout(() => {
			deps.activeHostTimers.delete(hostHandle);
			dispatchFire();
		}, timer.delayMs);

		timer.hostHandle = hostHandle;
		deps.activeHostTimers.add(hostHandle);
	};

	handlers.kernelTimerClear = (timerId: unknown) => {
		checkBridgeBudget(deps);
		const timer = deps.timerTable.get(Number(timerId));
		if (!timer || timer.pid !== deps.pid) return;

		if (timer.hostHandle !== undefined) {
			clearTimeout(timer.hostHandle as ReturnType<typeof setTimeout>);
			deps.activeHostTimers.delete(
				timer.hostHandle as ReturnType<typeof setTimeout>,
			);
			timer.hostHandle = undefined;
		}
		deps.timerTable.clearTimer(timer.id, deps.pid);
	};

	return handlers;
}

export interface KernelStdinDispatchDeps {
	liveStdinSource?: import("./isolate-bootstrap.js").LiveStdinSource;
	budgetState: BudgetState;
	maxBridgeCalls?: number;
}

export function buildKernelStdinDispatchHandlers(
	deps: KernelStdinDispatchDeps,
): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;

	handlers[K.kernelStdinRead] = async () => {
		checkBridgeBudget(deps);
		if (!deps.liveStdinSource) {
			return { done: true };
		}
		const chunk = await deps.liveStdinSource.read();
		if (chunk === null || chunk.length === 0) {
			return { done: true };
		}
		return {
			done: false,
			dataBase64: Buffer.from(chunk).toString("base64"),
		};
	};

	return handlers;
}

export interface KernelHandleDispatchDeps {
	processTable?: import("@secure-exec/core").ProcessTable;
	pid: number;
	budgetState: BudgetState;
	maxBridgeCalls?: number;
}

export function buildKernelHandleDispatchHandlers(
	deps: KernelHandleDispatchDeps,
): BridgeHandlers {
	const handlers: BridgeHandlers = {};

	handlers.kernelHandleRegister = (id: unknown, description: unknown) => {
		checkBridgeBudget(deps);
		if (!deps.processTable) return;

		const handleId = String(id);
		let activeHandles: Map<string, string>;
		try {
			activeHandles = deps.processTable.getHandles(deps.pid);
		} catch {
			return;
		}
		if (activeHandles.has(handleId)) {
			try {
				deps.processTable.unregisterHandle(deps.pid, handleId);
			} catch {
				// Process exit races turn re-register into a no-op.
			}
		}
		deps.processTable.registerHandle(deps.pid, handleId, String(description));
	};

	handlers.kernelHandleUnregister = (id: unknown) => {
		checkBridgeBudget(deps);
		if (!deps.processTable) return 0;

		try {
			deps.processTable.unregisterHandle(deps.pid, String(id));
		} catch {
			// Unknown handles already behave like a no-op at the bridge layer.
		}
		try {
			return deps.processTable.getHandles(deps.pid).size;
		} catch {
			return 0;
		}
	};

	handlers.kernelHandleList = () => {
		checkBridgeBudget(deps);
		if (!deps.processTable) return [];
		try {
			return Array.from(deps.processTable.getHandles(deps.pid).entries());
		} catch {
			return [];
		}
	};

	return handlers;
}

/** Dependencies for filesystem bridge handlers. */
export interface FsBridgeDeps {
	filesystem: VirtualFileSystem;
	budgetState: BudgetState;
	maxBridgeCalls?: number;
	bridgeBase64TransferLimitBytes: number;
	isolateJsonPayloadLimitBytes: number;
}

/** Build filesystem bridge handlers (readFile, writeFile, stat, etc.). */
export function buildFsBridgeHandlers(deps: FsBridgeDeps): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;
	const fs = deps.filesystem;
	const base64Limit = deps.bridgeBase64TransferLimitBytes;
	const jsonLimit = deps.isolateJsonPayloadLimitBytes;

	handlers[K.fsReadFile] = async (path: unknown) => {
		checkBridgeBudget(deps);
		const text = await readStandaloneProcAwareTextFile(fs, String(path));
		assertTextPayloadSize(`fs.readFile ${path}`, text, jsonLimit);
		return text;
	};

	handlers[K.fsWriteFile] = async (path: unknown, content: unknown) => {
		checkBridgeBudget(deps);
		await fs.writeFile(String(path), String(content));
	};

	handlers[K.fsReadFileBinary] = async (path: unknown) => {
		checkBridgeBudget(deps);
		const data = await readStandaloneProcAwareFile(fs, String(path));
		assertPayloadByteLength(`fs.readFileBinary ${path}`, getBase64EncodedByteLength(data.byteLength), base64Limit);
		return Buffer.from(data).toString("base64");
	};

	handlers[K.fsWriteFileBinary] = async (path: unknown, base64Content: unknown) => {
		checkBridgeBudget(deps);
		const b64 = String(base64Content);
		assertTextPayloadSize(`fs.writeFileBinary ${path}`, b64, base64Limit);
		await fs.writeFile(String(path), Buffer.from(b64, "base64"));
	};

	handlers[K.fsReadDir] = async (path: unknown) => {
		checkBridgeBudget(deps);
		const entries = (await fs.readDirWithTypes(String(path))).filter(
			(entry) => entry.name !== "." && entry.name !== "..",
		);
		const json = JSON.stringify(entries);
		assertTextPayloadSize(`fs.readDir ${path}`, json, jsonLimit);
		return json;
	};

	handlers[K.fsMkdir] = async (path: unknown) => {
		checkBridgeBudget(deps);
		await mkdir(fs, String(path));
	};

	handlers[K.fsRmdir] = async (path: unknown) => {
		checkBridgeBudget(deps);
		await fs.removeDir(String(path));
	};

	handlers[K.fsExists] = async (path: unknown) => {
		checkBridgeBudget(deps);
		return standaloneProcAwareExists(fs, String(path));
	};

	handlers[K.fsStat] = async (path: unknown) => {
		checkBridgeBudget(deps);
		const s = await standaloneProcAwareStat(fs, String(path));
		return JSON.stringify({ mode: s.mode, size: s.size, isDirectory: s.isDirectory,
			atimeMs: s.atimeMs, mtimeMs: s.mtimeMs, ctimeMs: s.ctimeMs, birthtimeMs: s.birthtimeMs });
	};

	handlers[K.fsUnlink] = async (path: unknown) => {
		checkBridgeBudget(deps);
		await fs.removeFile(String(path));
	};

	handlers[K.fsRename] = async (oldPath: unknown, newPath: unknown) => {
		checkBridgeBudget(deps);
		await fs.rename(String(oldPath), String(newPath));
	};

	handlers[K.fsChmod] = async (path: unknown, mode: unknown) => {
		checkBridgeBudget(deps);
		await fs.chmod(String(path), Number(mode));
	};

	handlers[K.fsChown] = async (path: unknown, uid: unknown, gid: unknown) => {
		checkBridgeBudget(deps);
		await fs.chown(String(path), Number(uid), Number(gid));
	};

	handlers[K.fsLink] = async (oldPath: unknown, newPath: unknown) => {
		checkBridgeBudget(deps);
		await fs.link(String(oldPath), String(newPath));
	};

	handlers[K.fsSymlink] = async (target: unknown, linkPath: unknown) => {
		checkBridgeBudget(deps);
		await fs.symlink(String(target), String(linkPath));
	};

	handlers[K.fsReadlink] = async (path: unknown) => {
		checkBridgeBudget(deps);
		return fs.readlink(String(path));
	};

	handlers[K.fsLstat] = async (path: unknown) => {
		checkBridgeBudget(deps);
		const s = await fs.lstat(String(path));
		return JSON.stringify({ mode: s.mode, size: s.size, isDirectory: s.isDirectory,
			isSymbolicLink: s.isSymbolicLink, atimeMs: s.atimeMs, mtimeMs: s.mtimeMs,
			ctimeMs: s.ctimeMs, birthtimeMs: s.birthtimeMs });
	};

	handlers[K.fsTruncate] = async (path: unknown, length: unknown) => {
		checkBridgeBudget(deps);
		await fs.truncate(String(path), Number(length));
	};

	handlers[K.fsUtimes] = async (path: unknown, atime: unknown, mtime: unknown) => {
		checkBridgeBudget(deps);
		await fs.utimes(String(path), Number(atime), Number(mtime));
	};

	return handlers;
}

/** Dependencies for child process bridge handlers. */
export interface ChildProcessBridgeDeps {
	commandExecutor: CommandExecutor;
	processConfig: ProcessConfig;
	budgetState: BudgetState;
	maxBridgeCalls?: number;
	maxChildProcesses?: number;
	isolateJsonPayloadLimitBytes: number;
	activeChildProcesses: Map<number, SpawnedProcess>;
	/** Push child process events into the V8 isolate. */
	sendStreamEvent: (eventType: string, payload: Uint8Array) => void;
	/** Kernel process table — when provided, child processes are registered for cross-runtime visibility. */
	processTable?: import("@secure-exec/core").ProcessTable;
	/** Parent process PID for kernel process table registration. */
	parentPid?: number;
}

/** Build child process bridge handlers. */
export function buildChildProcessBridgeHandlers(deps: ChildProcessBridgeDeps): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;
	const jsonLimit = deps.isolateJsonPayloadLimitBytes;
	let nextSessionId = 1;
	const sessions = deps.activeChildProcesses;
	const { processTable, parentPid } = deps;

	// Map sessionId → kernel PID for kernel-registered processes
	const sessionToPid = new Map<number, number>();

	/** Wrap a SpawnedProcess as a kernel DriverProcess (adds callback stubs). */
	function wrapAsDriverProcess(proc: SpawnedProcess) {
		return {
			writeStdin: (data: Uint8Array) => proc.writeStdin(data),
			closeStdin: () => proc.closeStdin(),
			kill: (signal: number) => proc.kill(signal),
			wait: () => proc.wait(),
			onStdout: null as ((data: Uint8Array) => void) | null,
			onStderr: null as ((data: Uint8Array) => void) | null,
			onExit: null as ((code: number) => void) | null,
		};
	}

	type ChildProcessStreamPayload =
		| { sessionId: number; dataBase64: string }
		| { sessionId: number; code: number };

	// Serialize a child process event and push it into the V8 isolate
	const dispatchEvent = (sessionId: number, type: "stdout" | "stderr" | "exit", data?: Uint8Array | number) => {
		try {
			let eventType: "child_stdout" | "child_stderr" | "child_exit";
			let payload: ChildProcessStreamPayload;
			if (type === "stdout" || type === "stderr") {
				eventType = type === "stdout" ? "child_stdout" : "child_stderr";
				payload = {
					sessionId,
					dataBase64: Buffer.from(data as Uint8Array).toString("base64"),
				};
			} else {
				eventType = "child_exit";
				payload = {
					sessionId,
					code: Number(data ?? 1),
				};
			}
			deps.sendStreamEvent(eventType, Buffer.from(JSON.stringify(payload)));
		} catch {
			// Context may be disposed
		}
	};

	handlers[K.childProcessSpawnStart] = (command: unknown, argsJson: unknown, optionsJson: unknown): number => {
		checkBridgeBudget(deps);
		if (deps.maxChildProcesses !== undefined && deps.budgetState.childProcesses >= deps.maxChildProcesses) {
			throw new Error(`${RESOURCE_BUDGET_ERROR_CODE}: maximum child processes exceeded`);
		}
		deps.budgetState.childProcesses++;
		const args = parseJsonWithLimit<string[]>("child_process.spawn args", String(argsJson), jsonLimit);
		const options = parseJsonWithLimit<{ cwd?: string; env?: Record<string, string> }>(
			"child_process.spawn options", String(optionsJson), jsonLimit);
		const sessionId = nextSessionId++;
		const childEnv = stripDangerousEnv(options.env ?? deps.processConfig.env);

		const proc = deps.commandExecutor.spawn(String(command), args, {
			cwd: options.cwd,
			env: childEnv,
			streamStdin: true,
			onStdout: (data) => dispatchEvent(sessionId, "stdout", data),
			onStderr: (data) => dispatchEvent(sessionId, "stderr", data),
		});

		// Register with kernel process table for cross-runtime visibility
		if (processTable && parentPid !== undefined) {
			const childPid = processTable.allocatePid();
			processTable.register(childPid, "node", String(command), args, {
				pid: childPid,
				ppid: parentPid,
				env: childEnv ?? {},
				cwd: options.cwd ?? deps.processConfig.cwd ?? "/",
				fds: { stdin: 0, stdout: 1, stderr: 2 },
			}, wrapAsDriverProcess(proc));
			sessionToPid.set(sessionId, childPid);
		}

		proc.wait().then((code) => {
			// Mark exited in kernel process table
			const childPid = sessionToPid.get(sessionId);
			if (childPid !== undefined && processTable) {
				try { processTable.markExited(childPid, code); } catch { /* already exited */ }
				sessionToPid.delete(sessionId);
			}
			dispatchEvent(sessionId, "exit", code);
			sessions.delete(sessionId);
		});

		sessions.set(sessionId, proc);
		return sessionId;
	};

	handlers[K.childProcessStdinWrite] = (sessionId: unknown, data: unknown) => {
		const d = data instanceof Uint8Array ? data : Buffer.from(String(data), "base64");
		const proc = sessions.get(Number(sessionId));
		proc?.writeStdin(d);
	};

	handlers[K.childProcessStdinClose] = (sessionId: unknown) => {
		sessions.get(Number(sessionId))?.closeStdin();
	};

	handlers[K.childProcessKill] = (sessionId: unknown, signal: unknown) => {
		const id = Number(sessionId);
		// Route through kernel process table when available
		const childPid = sessionToPid.get(id);
		if (childPid !== undefined && processTable) {
			try { processTable.kill(childPid, Number(signal)); } catch { /* already dead */ }
			return;
		}
		sessions.get(id)?.kill(Number(signal));
	};

	handlers[K.childProcessSpawnSync] = async (command: unknown, argsJson: unknown, optionsJson: unknown): Promise<string> => {
		checkBridgeBudget(deps);
		if (deps.maxChildProcesses !== undefined && deps.budgetState.childProcesses >= deps.maxChildProcesses) {
			throw new Error(`${RESOURCE_BUDGET_ERROR_CODE}: maximum child processes exceeded`);
		}
		deps.budgetState.childProcesses++;
		const args = parseJsonWithLimit<string[]>("child_process.spawnSync args", String(argsJson), jsonLimit);
		const options = parseJsonWithLimit<{ cwd?: string; env?: Record<string, string>; maxBuffer?: number }>(
			"child_process.spawnSync options", String(optionsJson), jsonLimit);

		const maxBuffer = options.maxBuffer ?? 1024 * 1024;
		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];
		let stdoutBytes = 0;
		let stderrBytes = 0;
		let maxBufferExceeded = false;

		const childEnv = stripDangerousEnv(options.env ?? deps.processConfig.env);

		const proc = deps.commandExecutor.spawn(String(command), args, {
			cwd: options.cwd,
			env: childEnv,
			onStdout: (data) => {
				if (maxBufferExceeded) return;
				stdoutBytes += data.length;
				if (maxBuffer !== undefined && stdoutBytes > maxBuffer) {
					maxBufferExceeded = true;
					proc.kill(15);
					return;
				}
				stdoutChunks.push(data);
			},
			onStderr: (data) => {
				if (maxBufferExceeded) return;
				stderrBytes += data.length;
				if (maxBuffer !== undefined && stderrBytes > maxBuffer) {
					maxBufferExceeded = true;
					proc.kill(15);
					return;
				}
				stderrChunks.push(data);
			},
		});

		// Register sync child with kernel process table
		let syncChildPid: number | undefined;
		if (processTable && parentPid !== undefined) {
			syncChildPid = processTable.allocatePid();
			processTable.register(syncChildPid, "node", String(command), args, {
				pid: syncChildPid,
				ppid: parentPid,
				env: childEnv ?? {},
				cwd: options.cwd ?? deps.processConfig.cwd ?? "/",
				fds: { stdin: 0, stdout: 1, stderr: 2 },
			}, wrapAsDriverProcess(proc));
		}

		const exitCode = await proc.wait();

		// Mark exited in kernel
		if (syncChildPid !== undefined && processTable) {
			try { processTable.markExited(syncChildPid, exitCode); } catch { /* already exited */ }
		}

		const decoder = new TextDecoder();
		const stdout = stdoutChunks.map((c) => decoder.decode(c)).join("");
		const stderr = stderrChunks.map((c) => decoder.decode(c)).join("");
		return JSON.stringify({ stdout, stderr, code: exitCode, maxBufferExceeded });
	};

	return handlers;
}

/** Dependencies for network bridge handlers. */
export interface NetworkBridgeDeps {
	networkAdapter: NetworkAdapter;
	budgetState: BudgetState;
	maxBridgeCalls?: number;
	isolateJsonPayloadLimitBytes: number;
	activeHttpServerIds: Set<number>;
	activeHttpServerClosers: Map<number, () => Promise<void>>;
	pendingHttpServerStarts: { count: number };
	activeHttpClientRequests: { count: number };
	/** Push HTTP server/upgrade events into the V8 isolate. */
	sendStreamEvent: (eventType: string, payload: Uint8Array) => void;
	/** Kernel socket table for all bridge-managed HTTP server routing. */
	socketTable?: import("@secure-exec/core").SocketTable;
	/** Process ID for kernel socket ownership. */
	pid?: number;
}

/** Result of building network bridge handlers — includes dispose for cleanup. */
export interface NetworkBridgeResult {
	handlers: BridgeHandlers;
	dispose: () => Promise<void>;
}

/** Restrict HTTP server hostname to loopback interfaces. */
function normalizeLoopbackHostname(hostname?: string): string {
	if (!hostname || hostname === "localhost") return "127.0.0.1";
	// Preserve wildcard binds so kernel listener lookup and server.address()
	// reflect the caller's requested address while loopback connects still
	// resolve through SocketTable wildcard matching.
	if (
		hostname === "127.0.0.1" ||
		hostname === "::1" ||
		hostname === "0.0.0.0" ||
		hostname === "::"
	) {
		return hostname;
	}
	throw new Error(
		`Sandbox HTTP servers are restricted to loopback interfaces. Received hostname: ${hostname}`,
	);
}

/** State for a kernel-routed HTTP server. */
interface KernelHttpServerState {
	listenSocketId: number;
	httpServer: http.Server;
	acceptLoopActive: boolean;
	closedPromise: Promise<void>;
	resolveClosed: () => void;
	pendingRequests: number;
	closeRequested: boolean;
	transportClosed: boolean;
}

function debugHttpBridge(...args: unknown[]): void {
	if (process.env.SECURE_EXEC_DEBUG_HTTP_BRIDGE === "1") {
		console.error("[secure-exec http bridge]", ...args);
	}
}

const MAX_REDIRECTS = 20;

type KernelHttpClientRequestOptions = {
	method?: string;
	headers?: Record<string, string>;
	body?: string | null;
	rejectUnauthorized?: boolean;
};

type KernelHttpClientResponse = Awaited<ReturnType<NetworkAdapter["httpRequest"]>> & {
	rawHeaders?: string[];
};

function shouldUseKernelHttpClientPath(
	adapter: NetworkAdapter,
	urlString: string,
): boolean {
	const loopbackAwareAdapter = adapter as NetworkAdapter & {
		__setLoopbackPortChecker?: (checker: (hostname: string, port: number) => boolean) => void;
	};
	if (typeof loopbackAwareAdapter.__setLoopbackPortChecker !== "function") {
		return false;
	}
	try {
		const parsed = new URL(urlString);
		return parsed.protocol === "http:" || parsed.protocol === "https:";
	} catch {
		return false;
	}
}

async function maybeDecompressHttpBody(
	buffer: Buffer,
	contentEncoding: string | string[] | undefined,
): Promise<Buffer> {
	const encoding = Array.isArray(contentEncoding)
		? contentEncoding[0]
		: contentEncoding;
	if (encoding !== "gzip" && encoding !== "deflate") {
		return buffer;
	}

	try {
		return await new Promise<Buffer>((resolve, reject) => {
			const decompress = encoding === "gzip" ? zlib.gunzip : zlib.inflate;
			decompress(buffer, (err, result) => {
				if (err) reject(err);
				else resolve(result);
			});
		});
	} catch {
		// Preserve the original bytes when decompression fails.
		return buffer;
	}
}

function shouldEncodeHttpBodyAsBinary(
	urlString: string,
	headers: http.IncomingHttpHeaders,
): boolean {
	const contentType = headers["content-type"] || "";
	const headerValue = Array.isArray(contentType) ? contentType.join(", ") : contentType;
	return (
		headerValue.includes("octet-stream") ||
		headerValue.includes("gzip") ||
		urlString.endsWith(".tgz")
	);
}

/**
 * Create a Duplex stream backed by a kernel socket.
 * Readable side reads from kernel socket readBuffer; writable side writes via send().
 */
function createKernelSocketDuplex(
	socketId: number,
	socketTable: import("@secure-exec/core").SocketTable,
	pid: number,
): Duplex {
	let readPumpStarted = false;

	const duplex = new Duplex({
		read() {
			if (readPumpStarted) return;
			readPumpStarted = true;
			runReadPump();
		},
		write(
			chunk: Buffer | string | Uint8Array,
			encoding: BufferEncoding,
			callback: (error?: Error | null) => void,
		) {
			try {
				const data = typeof chunk === "string"
					? Buffer.from(chunk, encoding)
					: Buffer.isBuffer(chunk)
						? chunk
						: Buffer.from(chunk);
				debugHttpBridge("socket write", socketId, data.length);
				socketTable.send(socketId, new Uint8Array(data), 0);
				callback();
			} catch (err) {
				debugHttpBridge("socket write error", socketId, err);
				// EBADF during TLS teardown: the kernel already closed this socket
				// (e.g. process killed while TLS handshake in progress). Silently
				// destroy the duplex instead of propagating the error through the
				// callback, which can become an uncaught exception inside
				// TLSSocket._start's synchronous uncork path.
				const errObj = err instanceof Error ? err : new Error(String(err));
				if ((errObj as any).code === "EBADF") {
					duplex.destroy();
					callback();
					return;
				}
				callback(errObj);
			}
		},
		final(callback: (error?: Error | null) => void) {
			try { socketTable.shutdown(socketId, "write"); } catch { /* already closed */ }
			callback();
		},
		destroy(err: Error | null, callback: (error?: Error | null) => void) {
			try { socketTable.close(socketId, pid); } catch { /* already closed */ }
			callback(err);
		},
	});

	// Socket-like properties for Node http module
	const socket = socketTable.get(socketId);
	const localAddr = socket?.localAddr;
	const remoteAddr = socket?.remoteAddr;
	(duplex as any).remoteAddress =
		remoteAddr && typeof remoteAddr === "object" && "host" in remoteAddr
			? remoteAddr.host
			: "127.0.0.1";
	(duplex as any).remotePort =
		remoteAddr && typeof remoteAddr === "object" && "port" in remoteAddr
			? remoteAddr.port
			: 0;
	(duplex as any).localAddress =
		localAddr && typeof localAddr === "object" && "host" in localAddr
			? localAddr.host
			: "127.0.0.1";
	(duplex as any).localPort =
		localAddr && typeof localAddr === "object" && "port" in localAddr
			? localAddr.port
			: 0;
	(duplex as any).encrypted = false;
	(duplex as any).setNoDelay = () => duplex;
	(duplex as any).setKeepAlive = () => duplex;
	(duplex as any).setTimeout = (ms: number, cb?: () => void) => {
		if (cb) duplex.once("timeout", cb);
		return duplex;
	};
	(duplex as any).ref = () => duplex;
	(duplex as any).unref = () => duplex;

	// Prevent uncaught exceptions from EBADF errors during TLS teardown.
	// When the kernel disposes sockets before TLS finishes its handshake,
	// the write callback propagates EBADF which becomes unhandled without this.
	duplex.on("error", (err: Error & { code?: string }) => {
		if (err.code === "EBADF") return;
		debugHttpBridge("socket duplex error", socketId, err);
	});

	async function runReadPump(): Promise<void> {
		try {
			while (true) {
				let data: Uint8Array | null;
				try {
					data = socketTable.recv(socketId, 65536, 0);
				} catch {
					break; // socket closed or error
				}

				if (data !== null) {
					debugHttpBridge("socket read", socketId, data.length);
					if (!duplex.push(Buffer.from(data))) {
						// Backpressure — wait for drain before continuing
						readPumpStarted = false;
						return;
					}
					continue;
				}

				// Check for EOF
				const sock = socketTable.get(socketId);
				if (!sock) break;
				if (sock.state === "closed" || sock.state === "read-closed") break;
				if (sock.peerWriteClosed || (sock.peerId === undefined && !sock.external)) break;
				if (sock.external && sock.readBuffer.length === 0 && sock.peerWriteClosed) break;

				// Wait for data
				const handle = sock.readWaiters.enqueue();
				await handle.wait();
			}
		} catch {
			// Socket destroyed during pump
		}
		duplex.push(null); // EOF
	}

	return duplex;
}

/** Build network bridge handlers (fetch, httpRequest, dnsLookup, httpServer). */
export function buildNetworkBridgeHandlers(deps: NetworkBridgeDeps): NetworkBridgeResult {
	if (!deps.socketTable || deps.pid === undefined) {
		throw new Error("buildNetworkBridgeHandlers requires a kernel socketTable and pid");
	}

	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;
	const adapter = deps.networkAdapter;
	const jsonLimit = deps.isolateJsonPayloadLimitBytes;
	const ownedHttpServers = new Set<number>();
	const ownedHttp2Servers = new Set<number>();
	const { socketTable, pid } = deps;

	// Track kernel HTTP servers for cleanup
	const kernelHttpServers = new Map<number, KernelHttpServerState>();
	type KernelHttp2ServerState = {
		listenSocketId: number;
		server: http2.Http2Server | http2.Http2SecureServer;
		sessions: Set<http2.ServerHttp2Session>;
		acceptLoopActive: boolean;
		closedPromise: Promise<void>;
		resolveClosed: () => void;
	};
	type SerializedHttp2SocketState = {
		encrypted?: boolean;
		allowHalfOpen?: boolean;
		localAddress?: string;
		localPort?: number;
		localFamily?: string;
		remoteAddress?: string;
		remotePort?: number;
		remoteFamily?: string;
		servername?: string;
		alpnProtocol?: string | false;
	};
	type SerializedHttp2SessionState = {
		encrypted?: boolean;
		alpnProtocol?: string | false;
		originSet?: string[];
		localSettings?: Record<string, boolean | number | Record<number, number>>;
		remoteSettings?: Record<string, boolean | number | Record<number, number>>;
		state?: {
			effectiveLocalWindowSize?: number;
			localWindowSize?: number;
			remoteWindowSize?: number;
			nextStreamID?: number;
			outboundQueueSize?: number;
			deflateDynamicTableSize?: number;
			inflateDynamicTableSize?: number;
		};
		socket?: SerializedHttp2SocketState;
	};
	type SerializedTlsDataValue =
		| { kind: "buffer"; data: string }
		| { kind: "string"; data: string };
	type SerializedTlsMaterial = SerializedTlsDataValue | SerializedTlsDataValue[];
	type SerializedTlsBridgeOptions = {
		isServer?: boolean;
		servername?: string;
		rejectUnauthorized?: boolean;
		requestCert?: boolean;
		session?: string;
		key?: SerializedTlsMaterial;
		cert?: SerializedTlsMaterial;
		ca?: SerializedTlsMaterial;
		passphrase?: string;
		ciphers?: string;
		ALPNProtocols?: string[];
		minVersion?: tls.SecureVersion;
		maxVersion?: tls.SecureVersion;
	};
	const kernelHttp2Servers = new Map<number, KernelHttp2ServerState>();
	type KernelHttp2ClientSessionState = {
		session: http2.ClientHttp2Session;
		closedPromise: Promise<void>;
		resolveClosed: () => void;
	};
	const kernelHttp2ClientSessions = new Map<number, KernelHttp2ClientSessionState>();
	const http2Sessions = new Map<number, http2.ClientHttp2Session | http2.ServerHttp2Session>();
	const http2Streams = new Map<number, http2.ClientHttp2Stream | http2.ServerHttp2Stream>();
	type PendingHttp2PushStreamState = {
		operations: Array<(stream: http2.ServerHttp2Stream) => void>;
	};
	const pendingHttp2PushStreams = new Map<number, PendingHttp2PushStreamState>();
	const http2ServerSessionIds = new WeakMap<http2.ServerHttp2Session, number>();
	let nextHttp2SessionId = 1;
	let nextHttp2StreamId = 1;
	const kernelUpgradeSockets = new Map<number, Duplex>();
	let nextKernelUpgradeSocketId = 1;
	const loopbackAwareAdapter = adapter as NetworkAdapter & {
		__setLoopbackPortChecker?: (checker: (hostname: string, port: number) => boolean) => void;
	};

	const decodeTlsMaterial = (
		value: SerializedTlsMaterial | undefined,
	): string | Buffer | Array<string | Buffer> | undefined => {
		if (value === undefined) {
			return undefined;
		}
		const decodeOne = (entry: SerializedTlsDataValue): string | Buffer =>
			entry.kind === "buffer" ? Buffer.from(entry.data, "base64") : entry.data;
		return Array.isArray(value) ? value.map(decodeOne) : decodeOne(value);
	};

	const buildHostTlsOptions = (
		options: SerializedTlsBridgeOptions | undefined,
	): Record<string, unknown> => {
		if (!options) {
			return {};
		}
		const hostOptions: Record<string, unknown> = {};
		const key = decodeTlsMaterial(options.key);
		const cert = decodeTlsMaterial(options.cert);
		const ca = decodeTlsMaterial(options.ca);
		if (key !== undefined) hostOptions.key = key;
		if (cert !== undefined) hostOptions.cert = cert;
		if (ca !== undefined) hostOptions.ca = ca;
		if (typeof options.passphrase === "string") hostOptions.passphrase = options.passphrase;
		if (typeof options.ciphers === "string") hostOptions.ciphers = options.ciphers;
		if (typeof options.session === "string") hostOptions.session = Buffer.from(options.session, "base64");
		if (Array.isArray(options.ALPNProtocols) && options.ALPNProtocols.length > 0) {
			hostOptions.ALPNProtocols = [...options.ALPNProtocols];
		}
		if (typeof options.minVersion === "string") hostOptions.minVersion = options.minVersion;
		if (typeof options.maxVersion === "string") hostOptions.maxVersion = options.maxVersion;
		if (typeof options.servername === "string") hostOptions.servername = options.servername;
		if (typeof options.requestCert === "boolean") hostOptions.requestCert = options.requestCert;
		if (typeof options.rejectUnauthorized === "boolean") {
			hostOptions.rejectUnauthorized = options.rejectUnauthorized;
		}
		return hostOptions;
	};

	const debugHttp2Bridge = (...args: unknown[]): void => {
		if (process.env.SECURE_EXEC_DEBUG_HTTP2_BRIDGE === "1") {
			console.error("[secure-exec http2 bridge]", ...args);
		}
	};

	const emitHttp2Event = (...fields: Array<string | number | undefined>): void => {
		const [kind, id, data, extra, extraNumber, extraHeaders, flags] = fields;
		debugHttp2Bridge("emit", kind, id);
		deps.sendStreamEvent("http2", Buffer.from(JSON.stringify({
			kind,
			id,
			data,
			extra,
			extraNumber,
			extraHeaders,
			flags,
		})));
	};

	const serializeHttp2SocketState = (
		socket: Pick<net.Socket, "localAddress" | "localPort" | "remoteAddress" | "remotePort" | "allowHalfOpen"> &
			Partial<tls.TLSSocket>,
	): string => JSON.stringify({
		encrypted: socket.encrypted === true,
		allowHalfOpen: socket.allowHalfOpen === true,
		localAddress: socket.localAddress,
		localPort: socket.localPort,
		localFamily: socket.localAddress?.includes(":") ? "IPv6" : "IPv4",
		remoteAddress: socket.remoteAddress,
		remotePort: socket.remotePort,
		remoteFamily: socket.remoteAddress?.includes(":") ? "IPv6" : "IPv4",
		servername:
			typeof (socket as tls.TLSSocket & { servername?: string }).servername === "string"
				? (socket as tls.TLSSocket & { servername?: string }).servername
				: undefined,
		alpnProtocol: socket.alpnProtocol || false,
	} satisfies SerializedHttp2SocketState);

	const serializeHttp2SessionState = (
		session: http2.ClientHttp2Session | http2.ServerHttp2Session,
	): string => JSON.stringify({
		encrypted: session.encrypted === true,
		alpnProtocol: session.alpnProtocol || (session.encrypted ? "h2" : "h2c"),
		originSet: Array.isArray(session.originSet) ? [...session.originSet] : undefined,
		localSettings:
			session.localSettings && typeof session.localSettings === "object"
				? session.localSettings as Record<string, boolean | number | Record<number, number>>
				: undefined,
		remoteSettings:
			session.remoteSettings && typeof session.remoteSettings === "object"
				? session.remoteSettings as Record<string, boolean | number | Record<number, number>>
				: undefined,
		state:
			session.state && typeof session.state === "object"
				? {
						effectiveLocalWindowSize:
							typeof session.state.effectiveLocalWindowSize === "number"
								? session.state.effectiveLocalWindowSize
								: undefined,
						localWindowSize:
							typeof session.state.localWindowSize === "number"
								? session.state.localWindowSize
								: undefined,
						remoteWindowSize:
							typeof session.state.remoteWindowSize === "number"
								? session.state.remoteWindowSize
								: undefined,
						nextStreamID:
							typeof session.state.nextStreamID === "number"
								? session.state.nextStreamID
								: undefined,
						outboundQueueSize:
							typeof session.state.outboundQueueSize === "number"
								? session.state.outboundQueueSize
								: undefined,
						deflateDynamicTableSize:
							typeof session.state.deflateDynamicTableSize === "number"
								? session.state.deflateDynamicTableSize
								: undefined,
						inflateDynamicTableSize:
							typeof session.state.inflateDynamicTableSize === "number"
								? session.state.inflateDynamicTableSize
								: undefined,
					}
				: undefined,
		socket: session.socket ? JSON.parse(serializeHttp2SocketState(session.socket as net.Socket & tls.TLSSocket)) : undefined,
	} satisfies SerializedHttp2SessionState);

	// Let host-side runtime.network.fetch/httpRequest reach only the HTTP
	// listeners owned by this execution.
	loopbackAwareAdapter.__setLoopbackPortChecker?.((_hostname, port) => {
		for (const state of kernelHttpServers.values()) {
			const socket = socketTable.get(state.listenSocketId);
			const localAddr = socket?.localAddr;
			if (localAddr && typeof localAddr === "object" && "port" in localAddr) {
				if (localAddr.port === port) {
					return true;
				}
			}
		}
		return false;
	});

	const performKernelHttpRequest = async (
		urlString: string,
		requestOptions: KernelHttpClientRequestOptions,
	): Promise<KernelHttpClientResponse> => {
		const url = new URL(urlString);
		const isHttps = url.protocol === "https:";
		const host = url.hostname;
		const port = Number(url.port || (isHttps ? 443 : 80));
		const socketId = socketTable.create(
			host.includes(":") ? AF_INET6 : AF_INET,
			SOCK_STREAM,
			0,
			pid,
		);
		await socketTable.connect(socketId, { host, port });

		const baseTransport = createKernelSocketDuplex(socketId, socketTable, pid);
		const requestTransport = isHttps
			? tls.connect({
				socket: baseTransport,
				servername: host,
				...(requestOptions.rejectUnauthorized !== undefined
					? { rejectUnauthorized: requestOptions.rejectUnauthorized }
					: {}),
			})
			: baseTransport;

		const transport = isHttps ? https : http;

		return await new Promise<KernelHttpClientResponse>((resolve, reject) => {
			let settled = false;
			const settleResolve = (value: KernelHttpClientResponse) => {
				if (settled) return;
				settled = true;
				resolve(value);
			};
			const settleReject = (error: unknown) => {
				if (settled) return;
				settled = true;
				reject(error);
			};

			const req = transport.request({
				hostname: host,
				port,
				path: `${url.pathname}${url.search}`,
				method: requestOptions.method || "GET",
				headers: requestOptions.headers || {},
				agent: false,
				createConnection: () => requestTransport,
			}, (res: http.IncomingMessage) => {
				const chunks: Buffer[] = [];
				res.on("data", (chunk: Buffer) => {
					chunks.push(chunk);
				});
				res.on("error", (error: Error) => {
					requestTransport.destroy();
					settleReject(error);
				});
				res.on("end", async () => {
					const decodedBuffer = await maybeDecompressHttpBody(
						Buffer.concat(chunks),
						res.headers["content-encoding"],
					);
					const buffer = Buffer.from(decodedBuffer);

					const headers: Record<string, string> = {};
					const rawHeaders = [...res.rawHeaders];
					Object.entries(res.headers).forEach(([key, value]) => {
						if (typeof value === "string") headers[key] = value;
						else if (Array.isArray(value)) headers[key] = value.join(", ");
					});
					delete headers["content-encoding"];

					const trailers: Record<string, string> = {};
					Object.entries(res.trailers || {}).forEach(([key, value]) => {
						if (typeof value === "string") trailers[key] = value;
					});

					const result: KernelHttpClientResponse = {
						status: res.statusCode || 200,
						statusText: res.statusMessage || "OK",
						headers,
						rawHeaders,
						url: urlString,
						body: shouldEncodeHttpBodyAsBinary(urlString, res.headers)
							? (() => {
								headers["x-body-encoding"] = "base64";
								return buffer.toString("base64");
							})()
							: buffer.toString("utf8"),
					};
					if (Object.keys(trailers).length > 0) {
						result.trailers = trailers;
					}
					requestTransport.destroy();
					settleResolve(result);
				});
			});

			req.on("upgrade", (res: http.IncomingMessage, upgradedSocket: Duplex, head: Buffer) => {
				const headers: Record<string, string> = {};
				const rawHeaders = [...res.rawHeaders];
				Object.entries(res.headers).forEach(([key, value]) => {
					if (typeof value === "string") headers[key] = value;
					else if (Array.isArray(value)) headers[key] = value.join(", ");
				});
				settleResolve({
					status: res.statusCode || 101,
					statusText: res.statusMessage || "Switching Protocols",
					headers,
					rawHeaders,
					body: head.toString("base64"),
					url: urlString,
					upgradeSocketId: registerKernelUpgradeSocket(upgradedSocket as Duplex),
				});
			});

			req.on("connect", (res: http.IncomingMessage, connectSocket: Duplex, head: Buffer) => {
				const headers: Record<string, string> = {};
				const rawHeaders = [...res.rawHeaders];
				Object.entries(res.headers).forEach(([key, value]) => {
					if (typeof value === "string") headers[key] = value;
					else if (Array.isArray(value)) headers[key] = value.join(", ");
				});
				settleResolve({
					status: res.statusCode || 200,
					statusText: res.statusMessage || "Connection established",
					headers,
					rawHeaders,
					body: head.toString("base64"),
					url: urlString,
					upgradeSocketId: registerKernelUpgradeSocket(connectSocket as Duplex),
				});
			});

			req.on("error", (error: Error) => {
				requestTransport.destroy();
				settleReject(error);
			});

			if (requestOptions.body) {
				req.write(requestOptions.body);
			}
			req.end();
		});
	};

	const performKernelFetch = async (
		urlString: string,
		requestOptions: KernelHttpClientRequestOptions,
	): Promise<Awaited<ReturnType<NetworkAdapter["fetch"]>>> => {
		let currentUrl = urlString;
		let redirected = false;
		let currentOptions = { ...requestOptions };

		for (let redirectCount = 0; redirectCount <= MAX_REDIRECTS; redirectCount += 1) {
			const response = await performKernelHttpRequest(currentUrl, currentOptions);
			if ([301, 302, 303, 307, 308].includes(response.status)) {
				const location = response.headers.location;
				if (location) {
					currentUrl = new URL(location, currentUrl).href;
					redirected = true;
					if (response.status === 301 || response.status === 302 || response.status === 303) {
						currentOptions = {
							...currentOptions,
							method: "GET",
							body: null,
						};
					}
					continue;
				}
			}

			return {
				ok: response.status >= 200 && response.status < 300,
				status: response.status,
				statusText: response.statusText,
				headers: { ...response.headers },
				body: response.body,
				url: currentUrl,
				redirected,
			};
		}

		throw new Error("Too many redirects");
	};

	const registerKernelUpgradeSocket = (socket: Duplex): number => {
		const socketId = nextKernelUpgradeSocketId++;
		kernelUpgradeSockets.set(socketId, socket);

		socket.on("data", (chunk) => {
			deps.sendStreamEvent("upgradeSocketData", Buffer.from(JSON.stringify({
				socketId,
				dataBase64: Buffer.from(chunk).toString("base64"),
			})));
		});
		socket.on("end", () => {
			deps.sendStreamEvent("upgradeSocketEnd", Buffer.from(JSON.stringify({ socketId })));
		});
		socket.on("close", () => {
			kernelUpgradeSockets.delete(socketId);
		});

		return socketId;
	};

	const finalizeKernelServerClose = (serverId: number, state: KernelHttpServerState): void => {
		debugHttpBridge("finalize close check", serverId, state.closeRequested, state.pendingRequests);
		if (!state.closeRequested || state.pendingRequests > 0) {
			return;
		}
		if (!state.transportClosed) {
			state.acceptLoopActive = false;
			state.transportClosed = true;
			try { socketTable?.close(state.listenSocketId, pid!); } catch { /* already closed */ }
			try { state.httpServer.close(); } catch { /* parser server is never bound */ }
		}
		debugHttpBridge("finalize close", serverId);
		state.resolveClosed();
		kernelHttpServers.delete(serverId);
		ownedHttpServers.delete(serverId);
		deps.activeHttpServerIds.delete(serverId);
		deps.activeHttpServerClosers.delete(serverId);
	};

	const closeKernelServer = async (serverId: number): Promise<void> => {
		const state = kernelHttpServers.get(serverId);
		if (!state) return;
		debugHttpBridge("close requested", serverId, state.pendingRequests);
		state.closeRequested = true;
		finalizeKernelServerClose(serverId, state);
	};

	handlers[K.networkFetchRaw] = async (url: unknown, optionsJson: unknown): Promise<string> => {
		checkBridgeBudget(deps);
		const options = parseJsonWithLimit<{ method?: string; headers?: Record<string, string>; body?: string | null }>(
			"network.fetch options", String(optionsJson), jsonLimit);
		deps.activeHttpClientRequests.count += 1;
		try {
			const urlString = String(url);
			const result = shouldUseKernelHttpClientPath(adapter, urlString)
				? await performKernelFetch(urlString, options)
				// Legacy fallback for custom adapters and explicit no-network stubs.
				: await adapter.fetch(urlString, options);
			const json = JSON.stringify(result);
			assertTextPayloadSize("network.fetch response", json, jsonLimit);
			return json;
		} finally {
			deps.activeHttpClientRequests.count = Math.max(0, deps.activeHttpClientRequests.count - 1);
		}
	};

	handlers[K.networkDnsLookupRaw] = async (hostname: unknown): Promise<string> => {
		checkBridgeBudget(deps);
		const result = await adapter.dnsLookup(String(hostname));
		return JSON.stringify(result);
	};

	handlers[K.networkHttpRequestRaw] = async (url: unknown, optionsJson: unknown): Promise<string> => {
		checkBridgeBudget(deps);
		const options = parseJsonWithLimit<{ method?: string; headers?: Record<string, string>; body?: string | null; rejectUnauthorized?: boolean }>(
			"network.httpRequest options", String(optionsJson), jsonLimit);
		deps.activeHttpClientRequests.count += 1;
		try {
			const urlString = String(url);
			const result = shouldUseKernelHttpClientPath(adapter, urlString)
				? await performKernelHttpRequest(urlString, options)
				// Legacy fallback for custom adapters and explicit no-network stubs.
				: await adapter.httpRequest(urlString, options);
			const json = JSON.stringify(result);
			assertTextPayloadSize("network.httpRequest response", json, jsonLimit);
			return json;
		} finally {
			deps.activeHttpClientRequests.count = Math.max(0, deps.activeHttpClientRequests.count - 1);
		}
	};

	handlers[K.networkHttpServerRespondRaw] = (
		serverId: unknown,
		requestId: unknown,
		responseJson: unknown,
	): void => {
		const numericServerId = Number(serverId);
		debugHttpBridge("respond callback", numericServerId, requestId);
		resolveHttpServerResponse({
			serverId: numericServerId,
			requestId: Number(requestId),
			responseJson: String(responseJson),
		});
		const state = kernelHttpServers.get(numericServerId);
		if (!state) {
			return;
		}
		state.pendingRequests = Math.max(0, state.pendingRequests - 1);
		finalizeKernelServerClose(numericServerId, state);
	};

	handlers[K.networkHttpServerWaitRaw] = async (serverId: unknown): Promise<void> => {
		const numericServerId = Number(serverId);
		debugHttpBridge("wait start", numericServerId);
		const state = kernelHttpServers.get(numericServerId);
		if (!state) {
			debugHttpBridge("wait missing", numericServerId);
			return;
		}
		await state.closedPromise;
		debugHttpBridge("wait resolved", numericServerId);
	};

	// HTTP server listen — always route through the kernel socket table
	handlers[K.networkHttpServerListenRaw] = (optionsJson: unknown): Promise<string> => {
		const options = parseJsonWithLimit<{ serverId: number; port?: number; hostname?: string }>(
			"network.httpServer.listen options", String(optionsJson), jsonLimit);
		deps.pendingHttpServerStarts.count += 1;

		return (async () => {
			try {
				const host = normalizeLoopbackHostname(options.hostname);
				debugHttpBridge("listen start", options.serverId, host, options.port ?? 0);
				const listenSocketId = socketTable.create(AF_INET, SOCK_STREAM, 0, pid);
				await socketTable.bind(listenSocketId, { host, port: options.port ?? 0 });
				await socketTable.listen(listenSocketId, 128, { external: true });

				// Get actual bound address (may differ for ephemeral port)
				const listenSocket = socketTable.get(listenSocketId);
				const addr = listenSocket?.localAddr as { host: string; port: number } | undefined;
				const address = addr ? {
					address: addr.host,
					family: addr.host.includes(":") ? "IPv6" : "IPv4",
					port: addr.port,
				} : null;

			// Create local HTTP server for parsing (not bound to any port)
			const httpServer = http.createServer(async (req, res) => {
				try {
					debugHttpBridge("request start", options.serverId, req.method, req.url);
					const chunks: Buffer[] = [];
					for await (const chunk of req) {
						chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
					}

					const headers: Record<string, string> = {};
					Object.entries(req.headers).forEach(([key, value]) => {
						if (typeof value === "string") headers[key] = value;
						else if (Array.isArray(value)) headers[key] = value[0] ?? "";
					});
					if (!headers.host && addr) {
						headers.host = `${addr.host}:${addr.port}`;
					}

					const requestJson = JSON.stringify({
						method: req.method || "GET",
						url: req.url || "/",
						headers,
						rawHeaders: req.rawHeaders || [],
						bodyBase64: chunks.length > 0
							? Buffer.concat(chunks).toString("base64")
							: undefined,
					});

					const requestId = nextHttpRequestId++;

					// Send request to sandbox and wait for response
					const responsePromise = new Promise<string>((resolve) => {
						registerPendingHttpResponse(options.serverId, requestId, resolve);
					});
					state.pendingRequests += 1;
					deps.sendStreamEvent("http_request", ipcSerialize({
						requestId,
						serverId: options.serverId,
						request: requestJson,
					}));
					const responseJson = await responsePromise;
					const response = parseJsonWithLimit<{
						status: number;
						headers?: Array<[string, string]>;
						rawHeaders?: string[];
						informational?: Array<{
							status: number;
							statusText?: string;
							headers?: Array<[string, string]>;
							rawHeaders?: string[];
						}>;
						body?: string;
						bodyEncoding?: "utf8" | "base64";
					}>("network.httpServer response", responseJson, jsonLimit);

					for (const informational of response.informational || []) {
						const rawHeaderLines = informational.rawHeaders && informational.rawHeaders.length > 0
							? informational.rawHeaders
							: (informational.headers || []).flatMap(([key, value]) => [key, value]);
						const statusText =
							informational.statusText ||
							http.STATUS_CODES[informational.status] ||
							"";
						const rawFrame =
							`HTTP/1.1 ${informational.status} ${statusText}\r\n` +
							rawHeaderLines.reduce((acc, entry, index) =>
								index % 2 === 0
									? `${acc}${entry}: ${rawHeaderLines[index + 1] ?? ""}\r\n`
									: acc,
							"") +
							"\r\n";
						(res as http.ServerResponse & { _writeRaw?: (chunk: string) => void })._writeRaw?.(rawFrame);
					}

					res.statusCode = response.status || 200;
					for (const [key, value] of response.headers || []) {
						res.setHeader(key, value);
					}

					if (response.body !== undefined) {
						if (response.bodyEncoding === "base64") {
							debugHttpBridge("response end", options.serverId, response.status, "base64", response.body.length);
							res.end(Buffer.from(response.body, "base64"));
						} else {
							debugHttpBridge("response end", options.serverId, response.status, "utf8", response.body.length);
							res.end(response.body);
						}
					} else {
						debugHttpBridge("response end", options.serverId, response.status, "empty", 0);
						res.end();
					}
				} catch {
					debugHttpBridge("request error", options.serverId, req.method, req.url);
					res.statusCode = 500;
					res.end("Internal Server Error");
				}
			});

			// Handle HTTP upgrades through kernel sockets
			httpServer.on("upgrade", (req, socket, head) => {
				const upgradeHeaders: Record<string, string> = {};
				Object.entries(req.headers).forEach(([key, value]) => {
					if (typeof value === "string") upgradeHeaders[key] = value;
					else if (Array.isArray(value)) upgradeHeaders[key] = value[0] ?? "";
				});
				const upgradeSocketId = registerKernelUpgradeSocket(socket as Duplex);
				deps.sendStreamEvent("httpServerUpgrade", Buffer.from(JSON.stringify({
					serverId: options.serverId,
					request: JSON.stringify({
						method: req.method || "GET",
						url: req.url || "/",
						headers: upgradeHeaders,
						rawHeaders: req.rawHeaders || [],
					}),
					head: head.toString("base64"),
					socketId: upgradeSocketId,
				})));
			});

			httpServer.on("connect", (req, socket, head) => {
				const connectHeaders: Record<string, string> = {};
				Object.entries(req.headers).forEach(([key, value]) => {
					if (typeof value === "string") connectHeaders[key] = value;
					else if (Array.isArray(value)) connectHeaders[key] = value[0] ?? "";
				});
				const connectSocketId = registerKernelUpgradeSocket(socket as Duplex);
				deps.sendStreamEvent("httpServerConnect", Buffer.from(JSON.stringify({
					serverId: options.serverId,
					request: JSON.stringify({
						method: req.method || "CONNECT",
						url: req.url || "/",
						headers: connectHeaders,
						rawHeaders: req.rawHeaders || [],
					}),
					head: head.toString("base64"),
					socketId: connectSocketId,
				})));
			});

				let resolveClosed!: () => void;
				const closedPromise = new Promise<void>((resolve) => {
					resolveClosed = resolve;
				});
				const state: KernelHttpServerState = {
					listenSocketId,
					httpServer,
					acceptLoopActive: true,
					closedPromise,
					resolveClosed,
					pendingRequests: 0,
					closeRequested: false,
					transportClosed: false,
				};
				debugHttpBridge("listen ready", options.serverId, address);
				kernelHttpServers.set(options.serverId, state);
				ownedHttpServers.add(options.serverId);
				deps.activeHttpServerIds.add(options.serverId);
				deps.activeHttpServerClosers.set(
					options.serverId,
					() => closeKernelServer(options.serverId),
				);

				// Start accept loop (fire-and-forget)
				void startKernelHttpAcceptLoop(state, socketTable, pid);

				return JSON.stringify({ address });
			} finally {
				deps.pendingHttpServerStarts.count -= 1;
			}
		})();
	};

	// HTTP server close — kernel-owned servers only
	handlers[K.networkHttpServerCloseRaw] = (serverId: unknown): Promise<void> => {
		const id = Number(serverId);
		debugHttpBridge("close bridge call", id);
		if (!ownedHttpServers.has(id)) {
			throw new Error(`Cannot close server ${id}: not owned by this execution context`);
		}

		const kernelState = kernelHttpServers.get(id);
		if (!kernelState) {
			throw new Error(`Cannot close server ${id}: kernel server state missing`);
		}
		return closeKernelServer(id);
	};

	const closeKernelHttp2Server = async (serverId: number): Promise<void> => {
		const state = kernelHttp2Servers.get(serverId);
		if (!state) {
			return;
		}
		state.acceptLoopActive = false;
		try {
			socketTable.close(state.listenSocketId, pid);
		} catch {
			// Listener already closed.
		}
		for (const session of [...state.sessions]) {
			try {
				session.close();
			} catch {
				// Ignore already-closing sessions.
			}
		}
		await new Promise<void>((resolve) => {
			try {
				state.server.close(() => resolve());
			} catch {
				resolve();
			}
		});
		kernelHttp2Servers.delete(serverId);
		ownedHttp2Servers.delete(serverId);
		deps.activeHttpServerIds.delete(serverId);
		deps.activeHttpServerClosers.delete(serverId);
		state.resolveClosed();
	};

	const startKernelHttp2AcceptLoop = async (
		state: KernelHttp2ServerState,
	): Promise<void> => {
		try {
			while (state.acceptLoopActive) {
				const listenSocket = socketTable.get(state.listenSocketId);
				if (!listenSocket || listenSocket.state !== "listening") {
					break;
				}

				const acceptedId = socketTable.accept(state.listenSocketId);
				if (acceptedId !== null) {
					const duplex = createKernelSocketDuplex(acceptedId, socketTable, pid);
					state.server.emit("connection", duplex);
					continue;
				}

				const handle = listenSocket.acceptWaiters.enqueue();
				const acceptedAfterEnqueue = socketTable.accept(state.listenSocketId);
				if (acceptedAfterEnqueue !== null) {
					handle.wake();
					const duplex = createKernelSocketDuplex(acceptedAfterEnqueue, socketTable, pid);
					state.server.emit("connection", duplex);
					continue;
				}

				await handle.wait();
			}
		} catch {
			// Listener closed.
		}
	};

	const normalizeHttp2EventHeaders = (
		headers: http2.IncomingHttpHeaders | http2.OutgoingHttpHeaders,
	): Record<string, string | string[] | number> => {
		const normalizedHeaders: Record<string, string | string[] | number> = {};
		for (const [key, value] of Object.entries(headers)) {
			if (value !== undefined) {
				normalizedHeaders[key] = value as string | string[] | number;
			}
		}
		return normalizedHeaders;
	};

	const emitHttp2SerializedError = (kind: string, id: number, error: unknown): void => {
		const err = error instanceof Error ? error : new Error(String(error));
		emitHttp2Event(kind, id, JSON.stringify({
			message: err.message,
			name: err.name,
			code: (err as { code?: unknown }).code,
		}));
	};

	const resolveHostHttp2FilePath = (filePath: string): string => {
		// The sandbox defaults process.execPath to /usr/bin/node, but the host-side
		// http2 respondWithFile helper needs a real host path when serving the Node binary.
		if (filePath === "/usr/bin/node" && process.execPath) {
			return process.execPath;
		}
		return filePath;
	};

	const withHttp2ServerStream = <T>(
		streamId: number,
		action: (stream: http2.ServerHttp2Stream) => T,
		fallback: () => T,
	): T => {
		const stream = http2Streams.get(streamId) as http2.ServerHttp2Stream | undefined;
		if (stream) {
			return action(stream);
		}
		const pending = pendingHttp2PushStreams.get(streamId);
		if (pending) {
			pending.operations.push((resolvedStream) => {
				action(resolvedStream);
			});
			return fallback();
		}
		throw new Error(`HTTP/2 stream ${String(streamId)} not found`);
	};

	const attachHttp2ClientStreamListeners = (
		streamId: number,
		stream: http2.ClientHttp2Stream,
	): void => {
		stream.on("response", (headers) => {
			emitHttp2Event(
				"clientResponseHeaders",
				streamId,
				JSON.stringify(normalizeHttp2EventHeaders(headers)),
			);
		});
		stream.on("push", (headers, flags) => {
			setImmediate(() => {
				emitHttp2Event(
					"clientPushHeaders",
					streamId,
					JSON.stringify(normalizeHttp2EventHeaders(headers)),
					undefined,
					String(flags ?? 0),
				);
			});
		});
		stream.on("data", (chunk) => {
			emitHttp2Event(
				"clientData",
				streamId,
				(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)).toString("base64"),
			);
		});
		stream.on("end", () => {
			debugHttp2Bridge("client response end", streamId);
			setImmediate(() => {
				emitHttp2Event("clientEnd", streamId);
			});
		});
		stream.on("close", () => {
			setImmediate(() => {
				emitHttp2Event("clientClose", streamId, undefined, undefined, String(stream.rstCode ?? 0));
				http2Streams.delete(streamId);
			});
		});
		stream.on("error", (error) => {
			emitHttp2SerializedError("clientError", streamId, error);
		});
		stream.resume();
	};

	const attachHttp2SessionListeners = (
		sessionId: number,
		session: http2.ClientHttp2Session | http2.ServerHttp2Session,
		onClose?: () => void,
	): void => {
		session.on("close", () => {
			debugHttp2Bridge("session close", sessionId);
			emitHttp2Event("sessionClose", sessionId);
			http2Sessions.delete(sessionId);
			onClose?.();
		});
		session.on("error", (error) => {
			debugHttp2Bridge("session error", sessionId, error instanceof Error ? error.message : String(error));
			emitHttp2SerializedError("sessionError", sessionId, error);
		});
		session.on("localSettings", (settings) => {
			emitHttp2Event("sessionLocalSettings", sessionId, JSON.stringify(settings));
		});
		session.on("remoteSettings", (settings) => {
			emitHttp2Event("sessionRemoteSettings", sessionId, JSON.stringify(settings));
		});
		session.on("goaway", (errorCode, lastStreamID, opaqueData) => {
			emitHttp2Event(
				"sessionGoaway",
				sessionId,
				Buffer.isBuffer(opaqueData) ? opaqueData.toString("base64") : undefined,
				undefined,
				String(errorCode),
				undefined,
				String(lastStreamID),
			);
		});
	};

	handlers[K.networkHttp2ServerListenRaw] = (optionsJson: unknown): Promise<string> => {
		const options = parseJsonWithLimit<{
			serverId: number;
			secure?: boolean;
			port?: number;
			host?: string;
			backlog?: number;
			allowHalfOpen?: boolean;
			allowHTTP1?: boolean;
			timeout?: number;
			settings?: Record<string, unknown>;
			remoteCustomSettings?: number[];
			tls?: SerializedTlsBridgeOptions;
		}>("network.http2Server.listen options", String(optionsJson), jsonLimit);

		return (async () => {
			debugHttp2Bridge("server listen start", options.serverId, options.secure, options.host, options.port);
			const host = normalizeLoopbackHostname(options.host);
			const listenSocketId = socketTable.create(AF_INET, SOCK_STREAM, 0, pid);
			await socketTable.bind(listenSocketId, { host, port: options.port ?? 0 });
			await socketTable.listen(listenSocketId, options.backlog ?? 128, { external: true });

			const listenSocket = socketTable.get(listenSocketId);
			const addr = listenSocket?.localAddr as { host: string; port: number } | undefined;
			const address = addr ? {
				address: addr.host,
				family: addr.host.includes(":") ? "IPv6" : "IPv4",
				port: addr.port,
			} : null;

			const server = options.secure
				? http2.createSecureServer({
						allowHTTP1: options.allowHTTP1 === true,
						settings: options.settings as http2.Settings,
						remoteCustomSettings: options.remoteCustomSettings,
						...buildHostTlsOptions(options.tls),
					} as http2.SecureServerOptions)
				: http2.createServer({
						allowHTTP1: options.allowHTTP1 === true,
						settings: options.settings as http2.Settings,
						remoteCustomSettings: options.remoteCustomSettings,
					} as http2.ServerOptions);

			if (typeof options.timeout === "number" && options.timeout > 0) {
				server.setTimeout(options.timeout);
			}

			server.on("timeout", () => {
				emitHttp2Event("serverTimeout", options.serverId);
			});
			server.on("connection", (socket) => {
				emitHttp2Event("serverConnection", options.serverId, serializeHttp2SocketState(socket));
			});
			if (options.secure) {
				server.on("secureConnection", (socket) => {
					emitHttp2Event("serverSecureConnection", options.serverId, serializeHttp2SocketState(socket));
				});
			}
			server.on("request", (req, res) => {
				if (req.httpVersionMajor === 2) {
					return;
				}
				void (async () => {
					const chunks: Buffer[] = [];
					for await (const chunk of req) {
						chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
					}

					const headers: Record<string, string> = {};
					Object.entries(req.headers).forEach(([key, value]) => {
						if (typeof value === "string") headers[key] = value;
						else if (Array.isArray(value)) headers[key] = value[0] ?? "";
					});

					const requestJson = JSON.stringify({
						method: req.method || "GET",
						url: req.url || "/",
						headers,
						rawHeaders: req.rawHeaders || [],
						bodyBase64: chunks.length > 0 ? Buffer.concat(chunks).toString("base64") : undefined,
					});
					const requestId = nextHttp2CompatRequestId++;
					const responsePromise = new Promise<string>((resolve) => {
						registerPendingHttp2CompatResponse(options.serverId, requestId, resolve);
					});
					emitHttp2Event("serverCompatRequest", options.serverId, requestJson, undefined, String(requestId));
					const responseJson = await responsePromise;
					const response = parseJsonWithLimit<{
						status: number;
						headers?: Array<[string, string]>;
						body?: string;
						bodyEncoding?: "utf8" | "base64";
					}>("network.http2Server.compat response", responseJson, jsonLimit);
					res.statusCode = response.status || 200;
					for (const [key, value] of response.headers || []) {
						res.setHeader(key, value);
					}
					if (response.bodyEncoding === "base64" && typeof response.body === "string") {
						res.end(Buffer.from(response.body, "base64"));
					} else if (typeof response.body === "string") {
						res.end(response.body);
					} else {
						res.end();
					}
				})().catch((error) => {
					try {
						res.statusCode = 500;
						res.end(error instanceof Error ? error.message : String(error));
					} catch {
						// Response already closed.
					}
				});
			});
			server.on("stream", (stream, headers, flags) => {
				debugHttp2Bridge("server stream", options.serverId, flags);
				const streamSession = stream.session as http2.ServerHttp2Session | undefined;
				if (!streamSession) {
					return;
				}
				let sessionId = http2ServerSessionIds.get(streamSession);
				if (sessionId === undefined) {
					sessionId = nextHttp2SessionId++;
					http2ServerSessionIds.set(streamSession, sessionId);
					http2Sessions.set(sessionId, streamSession);
					attachHttp2SessionListeners(sessionId, streamSession);
					emitHttp2Event("serverSession", options.serverId, serializeHttp2SessionState(streamSession), undefined, String(sessionId));
				}

				const streamId = nextHttp2StreamId++;
				http2Streams.set(streamId, stream);
				stream.pause();
				stream.on("data", (chunk) => {
					emitHttp2Event(
						"serverStreamData",
						streamId,
						(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)).toString("base64"),
					);
				});
				stream.on("end", () => {
					emitHttp2Event("serverStreamEnd", streamId);
				});
				stream.on("drain", () => {
					emitHttp2Event("serverStreamDrain", streamId);
				});
				stream.on("error", (error) => {
					emitHttp2SerializedError("serverStreamError", streamId, error);
				});
				stream.on("close", () => {
					emitHttp2Event("serverStreamClose", streamId, undefined, undefined, String(stream.rstCode ?? 0));
					http2Streams.delete(streamId);
				});
				emitHttp2Event(
					"serverStream",
					options.serverId,
					String(streamId),
					serializeHttp2SessionState(streamSession),
					String(sessionId),
					JSON.stringify(normalizeHttp2EventHeaders(headers)),
					String(flags ?? 0),
				);
			});
			server.on("close", () => {
				debugHttp2Bridge("server close", options.serverId);
				emitHttp2Event("serverClose", options.serverId);
			});

			let resolveClosed!: () => void;
			const closedPromise = new Promise<void>((resolve) => {
				resolveClosed = resolve;
			});
			const state: KernelHttp2ServerState = {
				listenSocketId,
				server,
				sessions: new Set<http2.ServerHttp2Session>(),
				acceptLoopActive: true,
				closedPromise,
				resolveClosed,
			};
			server.on("session", (session) => {
				state.sessions.add(session);
				session.once("close", () => {
					state.sessions.delete(session);
				});
			});
			kernelHttp2Servers.set(options.serverId, state);
			ownedHttp2Servers.add(options.serverId);
			deps.activeHttpServerIds.add(options.serverId);
			deps.activeHttpServerClosers.set(
				options.serverId,
				() => closeKernelHttp2Server(options.serverId),
			);
			void startKernelHttp2AcceptLoop(state);
			return JSON.stringify({ address });
		})();
	};

	handlers[K.networkHttp2ServerCloseRaw] = (serverId: unknown): Promise<void> => {
		const id = Number(serverId);
		if (!ownedHttp2Servers.has(id)) {
			throw new Error(`Cannot close HTTP/2 server ${id}: not owned by this execution context`);
		}
		return closeKernelHttp2Server(id);
	};

	handlers[K.networkHttp2ServerWaitRaw] = (serverId: unknown): Promise<void> => {
		const state = kernelHttp2Servers.get(Number(serverId));
		return state?.closedPromise ?? Promise.resolve();
	};

	handlers[K.networkHttp2SessionConnectRaw] = (optionsJson: unknown): Promise<string> => {
		const options = parseJsonWithLimit<{
			authority: string;
			protocol: string;
			host?: string;
			port?: number | string;
			localAddress?: string;
			family?: number;
			socketId?: number;
			settings?: Record<string, unknown>;
			remoteCustomSettings?: number[];
			tls?: SerializedTlsBridgeOptions;
		}>("network.http2Session.connect options", String(optionsJson), jsonLimit);

		return (async () => {
			const authority = String(options.authority);
			debugHttp2Bridge("session connect start", authority, options.socketId ?? null);
			const sessionId = nextHttp2SessionId++;
			let transport: Duplex;
			if (typeof options.socketId === "number") {
				transport = createKernelSocketDuplex(options.socketId, socketTable, pid);
			} else {
				const host = String(options.host ?? "127.0.0.1");
				const port = Number(options.port ?? 0);
				const socketId = socketTable.create(
					host.includes(":") ? AF_INET6 : AF_INET,
					SOCK_STREAM,
					0,
					pid,
				);
				if (typeof options.localAddress === "string" && options.localAddress.length > 0) {
					await socketTable.bind(socketId, {
						host: options.localAddress,
						port: 0,
					});
				}
				await socketTable.connect(socketId, { host, port });
				transport = createKernelSocketDuplex(socketId, socketTable, pid);
			}

			const session = http2.connect(authority, {
				settings: options.settings as http2.Settings,
				remoteCustomSettings: options.remoteCustomSettings,
				createConnection: () => {
					debugHttp2Bridge("createConnection", authority, options.protocol);
					if (options.protocol === "https:") {
						return tls.connect({
							socket: transport,
							ALPNProtocols: ["h2"],
							servername:
								typeof options.tls?.servername === "string" && options.tls.servername.length > 0
									? options.tls.servername
									: undefined,
							...buildHostTlsOptions(options.tls),
						});
					}
					return transport;
				},
			});

			let resolveClosed!: () => void;
			const closedPromise = new Promise<void>((resolve) => {
				resolveClosed = resolve;
			});
			http2Sessions.set(sessionId, session);
			kernelHttp2ClientSessions.set(sessionId, {
				session,
				closedPromise,
				resolveClosed,
			});
			session.on("connect", () => {
				debugHttp2Bridge("session connect", sessionId, authority);
				emitHttp2Event("sessionConnect", sessionId, serializeHttp2SessionState(session));
			});
			attachHttp2SessionListeners(sessionId, session, () => {
				kernelHttp2ClientSessions.get(sessionId)?.resolveClosed();
				kernelHttp2ClientSessions.delete(sessionId);
			});
			session.on("stream", (stream, headers, flags) => {
				const streamId = nextHttp2StreamId++;
				http2Streams.set(streamId, stream);
				attachHttp2ClientStreamListeners(streamId, stream);
				emitHttp2Event(
					"clientPushStream",
					sessionId,
					String(streamId),
					undefined,
					undefined,
					JSON.stringify(normalizeHttp2EventHeaders(headers)),
					String(flags ?? 0),
				);
			});

			return JSON.stringify({
				sessionId,
				state: serializeHttp2SessionState(session),
			});
		})();
	};

	handlers[K.networkHttp2SessionRequestRaw] = (
		sessionId: unknown,
		headersJson: unknown,
		optionsJson: unknown,
	): number => {
		const session = http2Sessions.get(Number(sessionId)) as http2.ClientHttp2Session | undefined;
		if (!session) {
			throw new Error(`HTTP/2 session ${String(sessionId)} not found`);
		}
		const headers = parseJsonWithLimit<Record<string, string | string[] | number>>(
			"network.http2Session.request headers",
			String(headersJson),
			jsonLimit,
		);
		const requestOptions = parseJsonWithLimit<Record<string, unknown>>(
			"network.http2Session.request options",
			String(optionsJson),
			jsonLimit,
		);
		const stream = session.request(headers, requestOptions as http2.ClientSessionRequestOptions);
		debugHttp2Bridge("session request", sessionId, stream.id);
		const streamId = nextHttp2StreamId++;
		http2Streams.set(streamId, stream);
		attachHttp2ClientStreamListeners(streamId, stream);
		return streamId;
	};

	handlers[K.networkHttp2SessionCloseRaw] = (sessionId: unknown): void => {
		http2Sessions.get(Number(sessionId))?.close();
	};

	handlers[K.networkHttp2SessionSettingsRaw] = (
		sessionId: unknown,
		settingsJson: unknown,
	): void => {
		const session = http2Sessions.get(Number(sessionId));
		if (!session) {
			throw new Error(`HTTP/2 session ${String(sessionId)} not found`);
		}
		const settings = parseJsonWithLimit<Record<string, unknown>>(
			"network.http2Session.settings settings",
			String(settingsJson),
			jsonLimit,
		);
		session.settings(settings as http2.Settings, () => {
			emitHttp2Event("sessionSettingsAck", Number(sessionId));
		});
	};

	handlers[K.networkHttp2SessionSetLocalWindowSizeRaw] = (
		sessionId: unknown,
		windowSize: unknown,
	): string => {
		const session = http2Sessions.get(Number(sessionId));
		if (!session) {
			throw new Error(`HTTP/2 session ${String(sessionId)} not found`);
		}
		session.setLocalWindowSize(Number(windowSize));
		return serializeHttp2SessionState(session);
	};

	handlers[K.networkHttp2SessionGoawayRaw] = (
		sessionId: unknown,
		errorCode: unknown,
		lastStreamID: unknown,
		opaqueDataBase64: unknown,
	): void => {
		const session = http2Sessions.get(Number(sessionId));
		if (!session) {
			throw new Error(`HTTP/2 session ${String(sessionId)} not found`);
		}
		session.goaway(
			Number(errorCode),
			Number(lastStreamID),
			typeof opaqueDataBase64 === "string" && opaqueDataBase64.length > 0
				? Buffer.from(opaqueDataBase64, "base64")
				: undefined,
		);
	};

	handlers[K.networkHttp2SessionDestroyRaw] = (sessionId: unknown): void => {
		http2Sessions.get(Number(sessionId))?.destroy();
	};

	handlers[K.networkHttp2SessionWaitRaw] = (sessionId: unknown): Promise<void> => {
		const state = kernelHttp2ClientSessions.get(Number(sessionId));
		return state?.closedPromise ?? Promise.resolve();
	};

	handlers[K.networkHttp2StreamRespondRaw] = (
		streamId: unknown,
		headersJson: unknown,
	): void => {
		const headers = parseJsonWithLimit<Record<string, string | string[] | number>>(
			"network.http2Stream.respond headers",
			String(headersJson),
			jsonLimit,
		);
		withHttp2ServerStream(
			Number(streamId),
			(stream) => {
				stream.respond(headers);
			},
			() => undefined,
		);
	};

	handlers[K.networkHttp2StreamPushStreamRaw] = (
		streamId: unknown,
		headersJson: unknown,
		optionsJson: unknown,
	): string => {
		const stream = http2Streams.get(Number(streamId)) as http2.ServerHttp2Stream | undefined;
		if (!stream) {
			throw new Error(`HTTP/2 stream ${String(streamId)} not found`);
		}
		const headers = parseJsonWithLimit<Record<string, string | string[] | number>>(
			"network.http2Stream.pushStream headers",
			String(headersJson),
			jsonLimit,
		);
		const options = parseJsonWithLimit<Record<string, unknown>>(
			"network.http2Stream.pushStream options",
			String(optionsJson),
			jsonLimit,
		);
		const pushStreamId = nextHttp2StreamId++;
		pendingHttp2PushStreams.set(pushStreamId, {
			operations: [],
		});
		stream.pushStream(
			headers,
			options as http2.StreamPriorityOptions,
			(error, pushStream, pushHeaders) => {
				const pending = pendingHttp2PushStreams.get(pushStreamId);
				if (error) {
					pendingHttp2PushStreams.delete(pushStreamId);
					emitHttp2SerializedError("serverStreamError", Number(streamId), error);
					return;
				}
				if (!pushStream) {
					pendingHttp2PushStreams.delete(pushStreamId);
					return;
				}
				http2Streams.set(pushStreamId, pushStream);
				pushStream.on("close", () => {
					http2Streams.delete(pushStreamId);
					pendingHttp2PushStreams.delete(pushStreamId);
				});
				for (const operation of pending?.operations ?? []) {
					operation(pushStream);
				}
				pendingHttp2PushStreams.delete(pushStreamId);
				void pushHeaders;
			},
		);
		return JSON.stringify({
			streamId: pushStreamId,
			headers: JSON.stringify(normalizeHttp2EventHeaders(headers)),
		});
	};

	handlers[K.networkHttp2StreamWriteRaw] = (
		streamId: unknown,
		dataBase64: unknown,
	): boolean => {
		return withHttp2ServerStream(
			Number(streamId),
			(stream) => stream.write(Buffer.from(String(dataBase64), "base64")),
			() => true,
		);
	};

	handlers[K.networkHttp2StreamEndRaw] = (
		streamId: unknown,
		dataBase64: unknown,
	): void => {
		withHttp2ServerStream(
			Number(streamId),
			(stream) => {
				if (typeof dataBase64 === "string" && dataBase64.length > 0) {
					stream.end(Buffer.from(dataBase64, "base64"));
					return;
				}
				stream.end();
			},
			() => undefined,
		);
	};

	handlers[K.networkHttp2StreamCloseRaw] = (
		streamId: unknown,
		rstCode: unknown,
	): void => {
		withHttp2ServerStream(
			Number(streamId),
			(stream) => {
				if (typeof (stream as { close?: (code?: number) => void }).close !== "function") {
					throw new Error(`HTTP/2 stream ${String(streamId)} not found`);
				}
				(stream as { close: (code?: number) => void }).close(
					typeof rstCode === "number" ? Number(rstCode) : undefined,
				);
			},
			() => undefined,
		);
	};

	handlers[K.networkHttp2StreamPauseRaw] = (streamId: unknown): void => {
		http2Streams.get(Number(streamId))?.pause();
	};

	handlers[K.networkHttp2StreamResumeRaw] = (streamId: unknown): void => {
		http2Streams.get(Number(streamId))?.resume();
	};

	handlers[K.networkHttp2StreamRespondWithFileRaw] = (
		streamId: unknown,
		filePath: unknown,
		headersJson: unknown,
		optionsJson: unknown,
	): void => {
		const headers = parseJsonWithLimit<Record<string, unknown>>(
			"network.http2Stream.respondWithFile headers",
			String(headersJson),
			jsonLimit,
		);
		const options = parseJsonWithLimit<Record<string, unknown>>(
			"network.http2Stream.respondWithFile options",
			String(optionsJson),
			jsonLimit,
		);
		withHttp2ServerStream(
			Number(streamId),
			(stream) => {
				stream.respondWithFile(
					resolveHostHttp2FilePath(String(filePath)),
					headers as http2.OutgoingHttpHeaders,
					options as http2.ServerStreamFileResponseOptionsWithError,
				);
			},
			() => undefined,
		);
	};

	handlers[K.networkHttp2ServerRespondRaw] = (
		serverId: unknown,
		requestId: unknown,
		responseJson: unknown,
	): void => {
		resolveHttp2CompatResponse({
			serverId: Number(serverId),
			requestId: Number(requestId),
			responseJson: String(responseJson),
		});
	};

	handlers[K.upgradeSocketWriteRaw] = (
		socketId: unknown,
		dataBase64: unknown,
	) => {
		const id = Number(socketId);
		const socket = kernelUpgradeSockets.get(id);
		if (socket) {
			socket.write(Buffer.from(String(dataBase64), "base64"));
			return;
		}
		adapter.upgradeSocketWrite?.(id, String(dataBase64));
	};

	handlers[K.upgradeSocketEndRaw] = (socketId: unknown) => {
		const id = Number(socketId);
		const socket = kernelUpgradeSockets.get(id);
		if (socket) {
			socket.end();
			return;
		}
		adapter.upgradeSocketEnd?.(id);
	};

	handlers[K.upgradeSocketDestroyRaw] = (socketId: unknown) => {
		const id = Number(socketId);
		const socket = kernelUpgradeSockets.get(id);
		if (socket) {
			kernelUpgradeSockets.delete(id);
			socket.destroy();
			return;
		}
		adapter.upgradeSocketDestroy?.(id);
	};

	// Register upgrade socket callbacks for httpRequest client-side upgrades
	adapter.setUpgradeSocketCallbacks?.({
		onData: (socketId, dataBase64) => {
			deps.sendStreamEvent("upgradeSocketData", Buffer.from(JSON.stringify({ socketId, dataBase64 })));
		},
		onEnd: (socketId) => {
			deps.sendStreamEvent("upgradeSocketEnd", Buffer.from(JSON.stringify({ socketId })));
		},
	});

	// Dispose: close all kernel HTTP servers
	const dispose = async (): Promise<void> => {
		for (const serverId of Array.from(kernelHttpServers.keys())) {
			await closeKernelServer(serverId);
		}
		for (const serverId of Array.from(kernelHttp2Servers.keys())) {
			await closeKernelHttp2Server(serverId);
		}
		for (const session of http2Sessions.values()) {
			try {
				session.destroy();
			} catch {
				// Session already closed.
			}
		}
		kernelHttp2ClientSessions.clear();
		http2Sessions.clear();
		http2Streams.clear();
		for (const socket of kernelUpgradeSockets.values()) {
			socket.destroy();
		}
		kernelUpgradeSockets.clear();
	};

	return { handlers, dispose };
}

/** Accept loop: dequeue connections from kernel listener and feed to http.Server. */
async function startKernelHttpAcceptLoop(
	state: KernelHttpServerState,
	socketTable: import("@secure-exec/core").SocketTable,
	pid: number,
): Promise<void> {
	try {
		while (state.acceptLoopActive) {
			const listenSocket = socketTable.get(state.listenSocketId);
			if (!listenSocket || listenSocket.state !== "listening") break;

			const acceptedId = socketTable.accept(state.listenSocketId);
			if (acceptedId !== null) {
				debugHttpBridge("accept backlog", state.listenSocketId, acceptedId);
				// Wrap kernel socket in Duplex and hand off to http.Server
				const duplex = createKernelSocketDuplex(acceptedId, socketTable, pid);
				state.httpServer.emit("connection", duplex);
				continue;
			}

			// Avoid a lost wake-up if a connection arrives between accept() and enqueue().
			const handle = listenSocket.acceptWaiters.enqueue();
			const acceptedAfterEnqueue = socketTable.accept(state.listenSocketId);
			if (acceptedAfterEnqueue !== null) {
				handle.wake();
				debugHttpBridge("accept after enqueue", state.listenSocketId, acceptedAfterEnqueue);
				const duplex = createKernelSocketDuplex(
					acceptedAfterEnqueue,
					socketTable,
					pid,
				);
				state.httpServer.emit("connection", duplex);
				continue;
			}

			// No pending connections — wait for accept waker
			await handle.wait();
		}
	} catch {
		// Listener closed — expected
	}
}

type PendingHttpResponse = {
	serverId: number;
	resolve: (response: string) => void;
};

type PendingHttp2CompatResponse = {
	serverId: number;
	resolve: (response: string) => void;
};

// Track request IDs directly, but also keep per-server FIFO queues so older
// callbacks that only report serverId still resolve the correct pending waiters.
const pendingHttpResponses = new Map<number, PendingHttpResponse>();
const pendingHttpResponsesByServer = new Map<number, number[]>();
let nextHttpRequestId = 1;
const pendingHttp2CompatResponses = new Map<number, PendingHttp2CompatResponse>();
const pendingHttp2CompatResponsesByServer = new Map<number, number[]>();
let nextHttp2CompatRequestId = 1;

function registerPendingHttpResponse(
	serverId: number,
	requestId: number,
	resolve: (response: string) => void,
): void {
	pendingHttpResponses.set(requestId, { serverId, resolve });
	const queue = pendingHttpResponsesByServer.get(serverId);
	if (queue) {
		queue.push(requestId);
	} else {
		pendingHttpResponsesByServer.set(serverId, [requestId]);
	}
}

function removePendingHttpResponse(serverId: number, requestId: number): PendingHttpResponse | undefined {
	const pending = pendingHttpResponses.get(requestId);
	if (!pending) return undefined;

	pendingHttpResponses.delete(requestId);

	const queue = pendingHttpResponsesByServer.get(serverId);
	if (queue) {
		const index = queue.indexOf(requestId);
		if (index !== -1) queue.splice(index, 1);
		if (queue.length === 0) pendingHttpResponsesByServer.delete(serverId);
	}

	return pending;
}

function takePendingHttpResponseByServer(serverId: number): PendingHttpResponse | undefined {
	const queue = pendingHttpResponsesByServer.get(serverId);
	if (!queue || queue.length === 0) return undefined;

	const requestId = queue.shift()!;
	if (queue.length === 0) pendingHttpResponsesByServer.delete(serverId);

	const pending = pendingHttpResponses.get(requestId);
	if (pending) {
		pendingHttpResponses.delete(requestId);
	}

	return pending;
}

function registerPendingHttp2CompatResponse(
	serverId: number,
	requestId: number,
	resolve: (response: string) => void,
): void {
	pendingHttp2CompatResponses.set(requestId, { serverId, resolve });
	const queue = pendingHttp2CompatResponsesByServer.get(serverId);
	if (queue) {
		queue.push(requestId);
	} else {
		pendingHttp2CompatResponsesByServer.set(serverId, [requestId]);
	}
}

function removePendingHttp2CompatResponse(
	serverId: number,
	requestId: number,
): PendingHttp2CompatResponse | undefined {
	const pending = pendingHttp2CompatResponses.get(requestId);
	if (!pending) return undefined;

	pendingHttp2CompatResponses.delete(requestId);

	const queue = pendingHttp2CompatResponsesByServer.get(serverId);
	if (queue) {
		const index = queue.indexOf(requestId);
		if (index !== -1) queue.splice(index, 1);
		if (queue.length === 0) pendingHttp2CompatResponsesByServer.delete(serverId);
	}

	return pending;
}

function takePendingHttp2CompatResponseByServer(
	serverId: number,
): PendingHttp2CompatResponse | undefined {
	const queue = pendingHttp2CompatResponsesByServer.get(serverId);
	if (!queue || queue.length === 0) return undefined;

	const requestId = queue.shift()!;
	if (queue.length === 0) pendingHttp2CompatResponsesByServer.delete(serverId);

	const pending = pendingHttp2CompatResponses.get(requestId);
	if (pending) {
		pendingHttp2CompatResponses.delete(requestId);
	}

	return pending;
}

/** Resolve a pending HTTP server response (called from stream callback handler). */
export function resolveHttpServerResponse(options: {
	requestId?: number;
	serverId?: number;
	responseJson: string;
}): void {
	const pending =
		options.requestId !== undefined
			? removePendingHttpResponse(
				options.serverId ?? pendingHttpResponses.get(options.requestId)?.serverId ?? -1,
				options.requestId,
			)
			: options.serverId !== undefined
				? takePendingHttpResponseByServer(options.serverId)
				: undefined;
	pending?.resolve(options.responseJson);
}

export function resolveHttp2CompatResponse(options: {
	requestId?: number;
	serverId?: number;
	responseJson: string;
}): void {
	const pending =
		options.requestId !== undefined
			? removePendingHttp2CompatResponse(
				options.serverId ?? pendingHttp2CompatResponses.get(options.requestId)?.serverId ?? -1,
				options.requestId,
			)
			: options.serverId !== undefined
				? takePendingHttp2CompatResponseByServer(options.serverId)
				: undefined;
	pending?.resolve(options.responseJson);
}

/** Dependencies for PTY bridge handlers. */
export interface PtyBridgeDeps {
	onPtySetRawMode?: (mode: boolean) => void;
	stdinIsTTY?: boolean;
}

/** Build PTY bridge handlers. */
export function buildPtyBridgeHandlers(deps: PtyBridgeDeps): BridgeHandlers {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;

	if (deps.stdinIsTTY && deps.onPtySetRawMode) {
		handlers[K.ptySetRawMode] = (mode: unknown) => {
			deps.onPtySetRawMode!(Boolean(mode));
		};
	}

	return handlers;
}

/** Dependencies for kernel FD table bridge handlers. */
export interface KernelFdBridgeDeps {
	filesystem: VirtualFileSystem;
	budgetState: BudgetState;
	maxBridgeCalls?: number;
}

/** Result of building kernel FD bridge handlers — includes dispose for cleanup. */
export interface KernelFdBridgeResult {
	handlers: BridgeHandlers;
	dispose: () => void;
}

const O_ACCMODE = 3;

function canRead(flags: number): boolean {
	const access = flags & O_ACCMODE;
	return access === O_RDONLY || access === O_RDWR;
}

function canWrite(flags: number): boolean {
	const access = flags & O_ACCMODE;
	return access === O_WRONLY || access === O_RDWR;
}

const PROC_SYS_KERNEL_HOSTNAME_PATH = "/proc/sys/kernel/hostname";

function getStandaloneProcFileContent(path: string): Uint8Array | null {
	if (path === PROC_SYS_KERNEL_HOSTNAME_PATH) {
		return Buffer.from("sandbox\n", "utf8");
	}
	return null;
}

function getStandaloneProcFileStat(
	path: string,
): import("@secure-exec/core").VirtualStat | null {
	const content = getStandaloneProcFileContent(path);
	if (!content) return null;
	const now = Date.now();
	return {
		mode: 0o100444,
		size: content.length,
		isDirectory: false,
		isSymbolicLink: false,
		atimeMs: now,
		mtimeMs: now,
		ctimeMs: now,
		birthtimeMs: now,
		ino: 0xfffe0001,
		nlink: 1,
		uid: 0,
		gid: 0,
	};
}

async function readStandaloneProcAwareFile(
	vfs: VirtualFileSystem,
	path: string,
): Promise<Uint8Array> {
	return getStandaloneProcFileContent(path) ?? vfs.readFile(path);
}

async function readStandaloneProcAwareTextFile(
	vfs: VirtualFileSystem,
	path: string,
): Promise<string> {
	const content = getStandaloneProcFileContent(path);
	if (content) return new TextDecoder().decode(content);
	return vfs.readTextFile(path);
}

async function standaloneProcAwareExists(
	vfs: VirtualFileSystem,
	path: string,
): Promise<boolean> {
	if (getStandaloneProcFileContent(path)) return true;
	return vfs.exists(path);
}

async function standaloneProcAwareStat(
	vfs: VirtualFileSystem,
	path: string,
): Promise<import("@secure-exec/core").VirtualStat> {
	return getStandaloneProcFileStat(path) ?? vfs.stat(path);
}

async function standaloneProcAwarePread(
	vfs: VirtualFileSystem,
	path: string,
	offset: number,
	length: number,
): Promise<Uint8Array> {
	const content = getStandaloneProcFileContent(path);
	if (content) {
		if (offset >= content.length) return new Uint8Array(0);
		return content.slice(offset, offset + length);
	}
	return vfs.pread(path, offset, length);
}

/**
 * Build kernel FD table bridge handlers.
 *
 * Creates a ProcessFDTable per execution and routes all FD operations
 * (open, close, read, write, fstat, ftruncate, fsync) through it.
 * The FD table tracks file descriptors, cursor positions, and flags.
 * Actual file I/O is delegated to the VirtualFileSystem.
 */
export function buildKernelFdBridgeHandlers(deps: KernelFdBridgeDeps): KernelFdBridgeResult {
	const handlers: BridgeHandlers = {};
	const K = HOST_BRIDGE_GLOBAL_KEYS;
	const vfs = deps.filesystem;

	// Create a per-execution FD table via the kernel FDTableManager
	const fdManager = new FDTableManager();
	const pid = 1;
	const fdTable = fdManager.create(pid);

	// fdOpen(path, flags, mode?) → fd number
	handlers[K.fdOpen] = async (path: unknown, flags: unknown, mode: unknown) => {
		checkBridgeBudget(deps);
		const pathStr = String(path);
		const numFlags = Number(flags);
		const numMode = mode !== undefined && mode !== null ? Number(mode) : undefined;

		const exists = await standaloneProcAwareExists(vfs, pathStr);

		// O_CREAT: create if doesn't exist
		if ((numFlags & O_CREAT) && !exists) {
			await vfs.writeFile(pathStr, "");
		} else if (!exists && !(numFlags & O_CREAT)) {
			throw new Error(`ENOENT: no such file or directory, open '${pathStr}'`);
		}

		// O_TRUNC: truncate existing file
		if ((numFlags & O_TRUNC) && exists) {
			await vfs.writeFile(pathStr, "");
		}

		const fd = fdTable.open(pathStr, numFlags, FILETYPE_REGULAR_FILE);

		// Store creation mode for umask application
		if (numMode !== undefined && (numFlags & O_CREAT)) {
			const entry = fdTable.get(fd);
			if (entry) entry.description.creationMode = numMode;
		}

		return fd;
	};

	// fdClose(fd)
	handlers[K.fdClose] = (fd: unknown) => {
		const fdNum = Number(fd);
		const ok = fdTable.close(fdNum);
		if (!ok) throw new Error("EBADF: bad file descriptor, close");
	};

	// fdRead(fd, length, position?) → base64 data
	handlers[K.fdRead] = async (fd: unknown, length: unknown, position: unknown) => {
		checkBridgeBudget(deps);
		const fdNum = Number(fd);
		const len = Number(length);
		const entry = fdTable.get(fdNum);
		if (!entry) throw new Error("EBADF: bad file descriptor, read");
		if (!canRead(entry.description.flags)) throw new Error("EBADF: bad file descriptor, read");

		const pos = (position !== null && position !== undefined)
			? Number(position)
			: Number(entry.description.cursor);

		const data = await standaloneProcAwarePread(vfs, entry.description.path, pos, len);

		// Update cursor only when no explicit position
		if (position === null || position === undefined) {
			entry.description.cursor += BigInt(data.length);
		}

		return Buffer.from(data).toString("base64");
	};

	// fdWrite(fd, base64data, position?) → bytes written
	handlers[K.fdWrite] = async (fd: unknown, base64data: unknown, position: unknown) => {
		checkBridgeBudget(deps);
		const fdNum = Number(fd);
		const entry = fdTable.get(fdNum);
		if (!entry) throw new Error("EBADF: bad file descriptor, write");
		if (!canWrite(entry.description.flags)) throw new Error("EBADF: bad file descriptor, write");

		const data = Buffer.from(String(base64data), "base64");

		// Read existing content
		let content: Uint8Array;
		try {
			content = await readStandaloneProcAwareFile(vfs, entry.description.path);
		} catch {
			content = new Uint8Array(0);
		}

		// Determine write position
		let writePos: number;
		if (entry.description.flags & O_APPEND) {
			writePos = content.length;
		} else if (position !== null && position !== undefined) {
			writePos = Number(position);
		} else {
			writePos = Number(entry.description.cursor);
		}

		// Splice data into content
		const endPos = writePos + data.length;
		const newContent = new Uint8Array(Math.max(content.length, endPos));
		newContent.set(content);
		newContent.set(data, writePos);
		await vfs.writeFile(entry.description.path, newContent);

		// Update cursor only when no explicit position
		if (position === null || position === undefined) {
			entry.description.cursor = BigInt(endPos);
		}

		return data.length;
	};

	// fdFstat(fd) → JSON stat string
	handlers[K.fdFstat] = async (fd: unknown) => {
		checkBridgeBudget(deps);
		const fdNum = Number(fd);
		const entry = fdTable.get(fdNum);
		if (!entry) throw new Error("EBADF: bad file descriptor, fstat");

		const stat = await standaloneProcAwareStat(vfs, entry.description.path);
		return JSON.stringify({
			dev: 0,
			ino: stat.ino ?? 0,
			mode: stat.mode,
			nlink: stat.nlink ?? 1,
			uid: stat.uid ?? 0,
			gid: stat.gid ?? 0,
			rdev: 0,
			size: stat.size,
			blksize: 4096,
			blocks: Math.ceil(stat.size / 512),
			atimeMs: stat.atimeMs ?? Date.now(),
			mtimeMs: stat.mtimeMs ?? Date.now(),
			ctimeMs: stat.ctimeMs ?? Date.now(),
			birthtimeMs: stat.birthtimeMs ?? Date.now(),
		});
	};

	// fdFtruncate(fd, len?)
	handlers[K.fdFtruncate] = async (fd: unknown, len: unknown) => {
		checkBridgeBudget(deps);
		const fdNum = Number(fd);
		const entry = fdTable.get(fdNum);
		if (!entry) throw new Error("EBADF: bad file descriptor, ftruncate");

		const newLen = (len !== undefined && len !== null) ? Number(len) : 0;
		let content: Uint8Array;
		try {
			content = await readStandaloneProcAwareFile(vfs, entry.description.path);
		} catch {
			content = new Uint8Array(0);
		}

		if (content.length > newLen) {
			await vfs.writeFile(entry.description.path, content.slice(0, newLen));
		} else if (content.length < newLen) {
			const padded = new Uint8Array(newLen);
			padded.set(content);
			await vfs.writeFile(entry.description.path, padded);
		}
	};

	// fdFsync(fd) — delegates to vfs.fsync if available, validates FD exists
	handlers[K.fdFsync] = async (fd: unknown) => {
		const fdNum = Number(fd);
		const entry = fdTable.get(fdNum);
		if (!entry) throw new Error("EBADF: bad file descriptor, fsync");
		await vfs.fsync?.(entry.description.path);
	};

	// fdGetPath(fd) → path string or null
	handlers[K.fdGetPath] = (fd: unknown) => {
		const fdNum = Number(fd);
		const entry = fdTable.get(fdNum);
		return entry ? entry.description.path : null;
	};

	return {
		handlers,
		dispose: () => {
			fdTable.closeAll();
		},
	};
}

export function createProcessConfigForExecution(
	processConfig: ProcessConfig,
	timingMitigation: string,
	frozenTimeMs: number,
): ProcessConfig {
	return {
		...processConfig,
		timingMitigation: timingMitigation as ProcessConfig["timingMitigation"],
		frozenTimeMs: timingMitigation === "freeze" ? frozenTimeMs : undefined,
	};
}
