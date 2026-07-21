import {
	cbc as nobleAesCbc,
	ctr as nobleAesCtr,
	gcm as nobleAesGcm,
} from "@noble/ciphers/aes.js";
import { hmac as nobleHmac } from "@noble/hashes/hmac.js";
import { md5, sha1 } from "@noble/hashes/legacy.js";
import { pbkdf2 as noblePbkdf2 } from "@noble/hashes/pbkdf2.js";
import { scrypt as nobleScrypt } from "@noble/hashes/scrypt.js";
import { sha224, sha256, sha384, sha512 } from "@noble/hashes/sha2.js";
import { transform } from "sucrase";
import type {
	BrowserChildProcessPollEvent,
	BrowserChildProcessSpawnRequest,
} from "./child-process-bridge.js";
import { createBrowserNetworkAdapter } from "./driver.js";
import { base64ToBytes, toUint8Array } from "./encoding.js";
import { posixErrno } from "./errno.js";
import type {
	ExecResult,
	NetworkAdapter,
	RunResult,
	StdioChannel,
	TimingMitigation,
	VirtualDirEntry,
	VirtualStat,
} from "./runtime.js";
import {
	createNetworkStub,
	exposeCustomGlobal,
	exposeMutableRuntimeStateGlobal,
	getIsolateRuntimeSource,
	getRequireSetupCode,
	getRuntimePolyfillCode,
	isESM,
	transformDynamicImport,
} from "./runtime.js";
import {
	defaultSignalExitCode,
	PROCESS_SIGNAL_NUMBERS,
	signalNumberForEvent,
} from "./signals.js";
import {
	assertBrowserSyncBridgeSupport,
	type BrowserSyncBridgeErrorPayload,
	type BrowserSyncBridgePayload,
	type BrowserWorkerSyncOperation,
	SYNC_BRIDGE_KIND_BINARY,
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
	SYNC_BRIDGE_SIGNAL_KIND_INDEX,
	SYNC_BRIDGE_SIGNAL_LENGTH_INDEX,
	SYNC_BRIDGE_SIGNAL_STATE_IDLE,
	SYNC_BRIDGE_SIGNAL_STATE_INDEX,
	SYNC_BRIDGE_SIGNAL_STATUS_INDEX,
	SYNC_BRIDGE_STATUS_ERROR,
} from "./sync-bridge.js";
import type {
	BrowserWorkerExecOptions,
	BrowserWorkerInitPayload,
	BrowserWorkerOutboundMessage,
	BrowserWorkerRequestMessage,
} from "./worker-protocol.js";

let networkAdapter: NetworkAdapter | null = null;
let initialized = false;
let controlToken: string | null = null;
let runtimeTimingMitigation: TimingMitigation = "freeze";
let runtimeProcessConfig: Record<string, unknown> | null = null;
let activeProcessRequestId: number | null = null;
let activeExecutionId: string | null = null;
let activeCaptureStdio = false;
let activeSyncBridge: ReturnType<typeof createSyncBridgeClient> | null = null;
const pendingExecutionSignals = new Map<string, (signal: number) => void>();

const dynamicImportCache = new Map<string, unknown>();
// For a PERSISTENT execution (ExecOptions.persistent): process.exit resolves this
// instead of throwing, so a long-running stdio program's async exit (e.g. an ACP agent
// exiting on stdin EOF, from an async callback that can't be caught by the sync exec
// wrapper) cleanly ends the run. Null in run-to-completion mode (exit throws as before).
let persistentExitResolver: ((code: number) => void) | null = null;
// Streaming stdin for a persistent execution: the host feeds more stdin while the program
// runs (and ends it explicitly), rather than the one-shot exec stdin that auto-ends. Lets
// the host drive a long-running stdio program (e.g. an ACP agent: write a request, read
// the reply, write the next) as a proper external client.
let streamingStdinEnabled = false;
let activeStdinPush: ((data: string) => void) | null = null;
let activeStdinEnd: (() => void) | null = null;
// Safety bound so a persistent program that never exits can't hang the worker.
const PERSISTENT_EXEC_TIMEOUT_MS = 120_000;
const MAX_ERROR_MESSAGE_CHARS = 8192;
const MAX_STDIO_MESSAGE_CHARS = 8192;

function eventForSignalNumber(signal: number): string {
	return (
		Object.entries(PROCESS_SIGNAL_NUMBERS).find(
			([, value]) => value === signal,
		)?.[0] ?? `SIG${signal}`
	);
}

// Payload size defaults matching the Node runtime path
const DEFAULT_BASE64_TRANSFER_BYTES = 16 * 1024 * 1024;
const DEFAULT_JSON_PAYLOAD_BYTES = 4 * 1024 * 1024;
const PAYLOAD_LIMIT_ERROR_CODE = "ERR_SANDBOX_PAYLOAD_TOO_LARGE";
const DEFAULT_SCRYPT_COST = 16_384;
const DEFAULT_SCRYPT_BLOCK_SIZE = 8;
const DEFAULT_SCRYPT_PARALLELIZATION = 1;

let base64TransferLimitBytes = DEFAULT_BASE64_TRANSFER_BYTES;
let jsonPayloadLimitBytes = DEFAULT_JSON_PAYLOAD_BYTES;

const encoder = new TextEncoder();
const decoder = new TextDecoder();
// biome-ignore lint/security/noGlobalEval: the browser worker intentionally evaluates isolated runtime source strings.
const globalEval = eval as (source: string) => unknown;
const SHARED_ARRAY_BUFFER_FREEZE_KEYS = [
	"byteLength",
	"slice",
	"grow",
	"maxByteLength",
	"growable",
] as const;

type TimingGlobalsSnapshot = {
	captured: boolean;
	dateDescriptor?: PropertyDescriptor;
	dateValue?: DateConstructor;
	performanceDescriptor?: PropertyDescriptor;
	performanceValue?: Performance;
	sharedArrayBufferDescriptor?: PropertyDescriptor;
	sharedArrayBufferValue?: typeof SharedArrayBuffer;
	sharedArrayBufferPrototypeDescriptors: Map<
		string,
		PropertyDescriptor | undefined
	>;
};

const timingGlobals: TimingGlobalsSnapshot = {
	captured: false,
	sharedArrayBufferPrototypeDescriptors: new Map(),
};

function getUtf8ByteLength(text: string): number {
	return encoder.encode(text).byteLength;
}

function getRequiredControlToken(): string {
	if (!controlToken) {
		throw new Error(
			"Browser runtime worker control channel is not initialized",
		);
	}
	return controlToken;
}

function captureTimingGlobals(): void {
	if (timingGlobals.captured) {
		return;
	}

	timingGlobals.captured = true;
	timingGlobals.dateDescriptor = Object.getOwnPropertyDescriptor(
		globalThis,
		"Date",
	);
	timingGlobals.dateValue = globalThis.Date;
	timingGlobals.performanceDescriptor = Object.getOwnPropertyDescriptor(
		globalThis,
		"performance",
	);
	timingGlobals.performanceValue = globalThis.performance;
	timingGlobals.sharedArrayBufferDescriptor = Object.getOwnPropertyDescriptor(
		globalThis,
		"SharedArrayBuffer",
	);
	timingGlobals.sharedArrayBufferValue = globalThis.SharedArrayBuffer;

	const sharedArrayBufferCtor = globalThis.SharedArrayBuffer;
	if (typeof sharedArrayBufferCtor !== "function") {
		return;
	}

	const prototype = sharedArrayBufferCtor.prototype as Record<string, unknown>;
	for (const key of SHARED_ARRAY_BUFFER_FREEZE_KEYS) {
		timingGlobals.sharedArrayBufferPrototypeDescriptors.set(
			key,
			Object.getOwnPropertyDescriptor(prototype, key),
		);
	}
}

function restoreGlobalProperty(
	name: "Date" | "performance" | "SharedArrayBuffer",
	descriptor?: PropertyDescriptor,
): void {
	if (descriptor) {
		try {
			Object.defineProperty(globalThis, name, descriptor);
			return;
		} catch {
			if ("value" in descriptor) {
				(globalThis as Record<string, unknown>)[name] = descriptor.value;
				return;
			}
		}
	}

	Reflect.deleteProperty(globalThis, name);
}

function restoreSharedArrayBufferPrototype(): void {
	const sharedArrayBufferCtor = timingGlobals.sharedArrayBufferValue;
	if (typeof sharedArrayBufferCtor !== "function") {
		return;
	}

	const prototype = sharedArrayBufferCtor.prototype as Record<string, unknown>;
	for (const key of SHARED_ARRAY_BUFFER_FREEZE_KEYS) {
		const descriptor =
			timingGlobals.sharedArrayBufferPrototypeDescriptors.get(key);
		try {
			if (descriptor) {
				Object.defineProperty(prototype, key, descriptor);
			} else {
				delete prototype[key];
			}
		} catch {
			// Ignore non-configurable SharedArrayBuffer prototype properties.
		}
	}
}

function restoreTimingMitigationOff(): void {
	captureTimingGlobals();
	restoreGlobalProperty("Date", timingGlobals.dateDescriptor);
	restoreGlobalProperty("performance", timingGlobals.performanceDescriptor);
	restoreSharedArrayBufferPrototype();
	restoreGlobalProperty(
		"SharedArrayBuffer",
		timingGlobals.sharedArrayBufferDescriptor,
	);

	if (
		typeof globalThis.performance === "undefined" ||
		globalThis.performance === null
	) {
		Object.defineProperty(globalThis, "performance", {
			value: {
				now: () => Date.now(),
			},
			configurable: true,
			writable: true,
		});
	}
}

function applyTimingMitigation(
	timingMitigation: TimingMitigation,
	frozenTimeMs?: number,
): number | undefined {
	captureTimingGlobals();
	restoreTimingMitigationOff();
	if (timingMitigation !== "freeze") {
		return undefined;
	}

	const frozenTimeValue =
		typeof frozenTimeMs === "number" && Number.isFinite(frozenTimeMs)
			? Math.trunc(frozenTimeMs)
			: Date.now();
	const originalDate =
		timingGlobals.dateValue ?? timingGlobals.dateDescriptor?.value ?? Date;
	const frozenDateNow = () => frozenTimeValue;
	const FrozenDate = function (...args: unknown[]) {
		if (new.target) {
			if (args.length === 0) {
				return new originalDate(frozenTimeValue);
			}
			return new originalDate(
				...(args as ConstructorParameters<DateConstructor>),
			);
		}
		return originalDate();
	} as unknown as DateConstructor;
	Object.defineProperty(FrozenDate, "prototype", {
		value: originalDate.prototype,
		writable: false,
		configurable: false,
	});
	Object.defineProperty(FrozenDate, "now", {
		value: frozenDateNow,
		configurable: true,
		writable: false,
	});
	FrozenDate.parse = originalDate.parse;
	FrozenDate.UTC = originalDate.UTC;
	try {
		Object.defineProperty(globalThis, "Date", {
			value: FrozenDate,
			configurable: true,
			writable: false,
		});
	} catch {
		(globalThis as Record<string, unknown>).Date = FrozenDate;
	}

	const frozenPerformance = Object.create(null) as Record<string, unknown>;
	const originalPerformance = timingGlobals.performanceValue;
	if (
		typeof originalPerformance !== "undefined" &&
		originalPerformance !== null
	) {
		const source = originalPerformance as unknown as Record<string, unknown>;
		for (const key of Object.getOwnPropertyNames(
			Object.getPrototypeOf(originalPerformance) ?? originalPerformance,
		)) {
			if (key === "now") {
				continue;
			}
			try {
				const value = source[key];
				frozenPerformance[key] =
					typeof value === "function" ? value.bind(originalPerformance) : value;
			} catch {
				// Ignore performance accessors that throw in this host.
			}
		}
	}
	Object.defineProperty(frozenPerformance, "now", {
		value: () => 0,
		configurable: true,
		writable: false,
	});
	Object.freeze(frozenPerformance);
	try {
		Object.defineProperty(globalThis, "performance", {
			value: frozenPerformance,
			configurable: true,
			writable: false,
		});
	} catch {
		(globalThis as Record<string, unknown>).performance = frozenPerformance;
	}

	const sharedArrayBufferCtor = timingGlobals.sharedArrayBufferValue;
	if (typeof sharedArrayBufferCtor === "function") {
		const prototype = sharedArrayBufferCtor.prototype as Record<
			string,
			unknown
		>;
		for (const key of SHARED_ARRAY_BUFFER_FREEZE_KEYS) {
			try {
				Object.defineProperty(prototype, key, {
					get() {
						throw new TypeError(
							"SharedArrayBuffer is not available in sandbox",
						);
					},
					configurable: true,
				});
			} catch {
				// Ignore non-configurable SharedArrayBuffer prototype properties.
			}
		}
	}
	try {
		Object.defineProperty(globalThis, "SharedArrayBuffer", {
			value: undefined,
			configurable: true,
			writable: false,
			enumerable: false,
		});
	} catch {
		Reflect.deleteProperty(globalThis, "SharedArrayBuffer");
	}

	return frozenTimeValue;
}

function assertPayloadByteLength(
	payloadLabel: string,
	actualBytes: number,
	maxBytes: number,
): void {
	if (actualBytes <= maxBytes) return;
	const error = new Error(
		`[${PAYLOAD_LIMIT_ERROR_CODE}] ${payloadLabel}: payload is ${actualBytes} bytes, limit is ${maxBytes} bytes`,
	);
	(error as { code?: string }).code = PAYLOAD_LIMIT_ERROR_CODE;
	throw error;
}

function assertTextPayloadSize(
	payloadLabel: string,
	text: string,
	maxBytes: number,
): void {
	assertPayloadByteLength(payloadLabel, getUtf8ByteLength(text), maxBytes);
}

interface BrowserScryptOptions {
	cost?: unknown;
	N?: unknown;
	blockSize?: unknown;
	r?: unknown;
	parallelization?: unknown;
	p?: unknown;
}

function parseScryptOptions(value: unknown): BrowserScryptOptions {
	if (value == null) return {};
	if (typeof value === "string") {
		const parsed = JSON.parse(value);
		return typeof parsed === "object" && parsed !== null
			? (parsed as BrowserScryptOptions)
			: {};
	}
	return typeof value === "object" ? (value as BrowserScryptOptions) : {};
}

function normalizeScryptPositiveInteger(
	value: unknown,
	fallback: number,
	label: string,
): number {
	const normalized = value == null ? fallback : Number(value);
	if (!Number.isInteger(normalized) || normalized <= 0) {
		throw new Error(`crypto.scrypt ${label} must be a positive integer`);
	}
	return normalized;
}

function normalizeScryptOptions(value: unknown, keyLength: unknown) {
	const options = parseScryptOptions(value);
	const length = Number(keyLength);
	if (!Number.isInteger(length) || length < 0) {
		throw new Error("crypto.scrypt key length must be a non-negative integer");
	}
	const cost = normalizeScryptPositiveInteger(
		options.cost ?? options.N,
		DEFAULT_SCRYPT_COST,
		"cost",
	);
	if ((cost & (cost - 1)) !== 0) {
		throw new Error("crypto.scrypt cost must be a positive power of two");
	}
	return {
		N: cost,
		r: normalizeScryptPositiveInteger(
			options.blockSize ?? options.r,
			DEFAULT_SCRYPT_BLOCK_SIZE,
			"block size",
		),
		p: normalizeScryptPositiveInteger(
			options.parallelization ?? options.p,
			DEFAULT_SCRYPT_PARALLELIZATION,
			"parallelization",
		),
		dkLen: length,
	};
}

const BROWSER_CRYPTO_HASHES = {
	md5,
	sha1,
	sha224,
	sha256,
	sha384,
	sha512,
};

type BrowserCryptoHashName = keyof typeof BROWSER_CRYPTO_HASHES;

function normalizeCryptoHashName(algorithm: unknown): BrowserCryptoHashName {
	const normalized = String(algorithm)
		.trim()
		.toLowerCase()
		.replace(/[-_]/g, "");
	if (Object.hasOwn(BROWSER_CRYPTO_HASHES, normalized)) {
		return normalized as BrowserCryptoHashName;
	}
	throw new Error(`Unsupported browser crypto digest algorithm: ${algorithm}`);
}

const RSA_PKCS1_DIGEST_PREFIXES: Record<BrowserCryptoHashName, string> = {
	md5: "3020300c06082a864886f70d020505000410",
	sha1: "3021300906052b0e03021a05000414",
	sha224: "302d300d06096086480165030402040500041c",
	sha256: "3031300d060960864801650304020105000420",
	sha384: "3041300d060960864801650304020205000430",
	sha512: "3051300d060960864801650304020305000440",
};

function browserCryptoHash(algorithm: unknown) {
	return BROWSER_CRYPTO_HASHES[normalizeCryptoHashName(algorithm)];
}

function hashDigestBytes(algorithm: unknown, data: unknown): Uint8Array {
	return browserCryptoHash(algorithm)(toUint8Array(data));
}

function hmacDigestBytes(
	algorithm: unknown,
	key: unknown,
	data: unknown,
): Uint8Array {
	return nobleHmac(
		browserCryptoHash(algorithm),
		toUint8Array(key),
		toUint8Array(data),
	);
}

function normalizeSignatureHashName(algorithm: unknown): BrowserCryptoHashName {
	const normalized = String(algorithm)
		.trim()
		.toLowerCase()
		.replace(/^rsa[-_]/, "");
	return normalizeCryptoHashName(normalized);
}

function bytesToHex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(
		"",
	);
}

function hexToBytes(hex: string): Uint8Array {
	const out = new Uint8Array(hex.length / 2);
	for (let index = 0; index < out.length; index++) {
		out[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
	}
	return out;
}

function concatBytes(chunks: Uint8Array[]): Uint8Array {
	const total = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return out;
}

function pemToDerBytes(value: unknown): Uint8Array {
	let pem: string;
	if (typeof value === "string") {
		try {
			const parsed = JSON.parse(value) as unknown;
			pem = typeof parsed === "string" ? parsed : value;
		} catch {
			pem = value;
		}
	} else if (value && typeof value === "object" && "key" in value) {
		return pemToDerBytes((value as { key?: unknown }).key);
	} else {
		throw new Error("Browser crypto RSA key must be a PEM string");
	}
	const base64 = pem.replace(
		/-----BEGIN [^-]+-----|-----END [^-]+-----|\s+/g,
		"",
	);
	return base64ToBytes(base64);
}

class DerReader {
	private offset = 0;
	constructor(private readonly bytes: Uint8Array) {}

	get position(): number {
		return this.offset;
	}

	set position(value: number) {
		this.offset = value;
	}

	readTag(expected: number): Uint8Array {
		const tag = this.bytes[this.offset++];
		if (tag !== expected) {
			throw new Error(
				`Invalid RSA key DER tag: expected ${expected}, got ${tag}`,
			);
		}
		const length = this.readLength();
		const end = this.offset + length;
		if (end > this.bytes.byteLength) {
			throw new Error("Invalid RSA key DER length");
		}
		const value = this.bytes.subarray(this.offset, end);
		this.offset = end;
		return value;
	}

	readSequence(): DerReader {
		return new DerReader(this.readTag(0x30));
	}

	readInteger(): Uint8Array {
		let value = this.readTag(0x02);
		while (value.byteLength > 1 && value[0] === 0) {
			value = value.subarray(1);
		}
		return value;
	}

	readOctetString(): Uint8Array {
		return this.readTag(0x04);
	}

	readBitString(): Uint8Array {
		const value = this.readTag(0x03);
		if (value[0] !== 0) {
			throw new Error("Unsupported RSA key bit string padding");
		}
		return value.subarray(1);
	}

	skipAny(): void {
		const tag = this.bytes[this.offset++];
		if (tag == null) {
			throw new Error("Invalid RSA key DER");
		}
		const length = this.readLength();
		this.offset += length;
		if (this.offset > this.bytes.byteLength) {
			throw new Error("Invalid RSA key DER length");
		}
	}

	private readLength(): number {
		const first = this.bytes[this.offset++];
		if (first == null) {
			throw new Error("Invalid RSA key DER length");
		}
		if ((first & 0x80) === 0) {
			return first;
		}
		const lengthBytes = first & 0x7f;
		if (lengthBytes === 0 || lengthBytes > 4) {
			throw new Error("Unsupported RSA key DER length");
		}
		let length = 0;
		for (let index = 0; index < lengthBytes; index++) {
			length = (length << 8) | this.bytes[this.offset++];
		}
		return length;
	}
}

type BrowserRsaKey = {
	modulus: bigint;
	exponent: bigint;
	modulusLength: number;
};

function bytesToBigInt(bytes: Uint8Array): bigint {
	const hex = bytesToHex(bytes);
	return hex.length === 0 ? 0n : BigInt(`0x${hex}`);
}

function bigIntToBytes(value: bigint, length: number): Uint8Array {
	let hex = value.toString(16);
	if (hex.length % 2 !== 0) {
		hex = `0${hex}`;
	}
	const bytes = hexToBytes(hex);
	if (bytes.byteLength > length) {
		throw new Error("RSA value exceeds modulus length");
	}
	const out = new Uint8Array(length);
	out.set(bytes, length - bytes.byteLength);
	return out;
}

function modPow(base: bigint, exponent: bigint, modulus: bigint): bigint {
	let result = 1n;
	let factor = base % modulus;
	let power = exponent;
	while (power > 0n) {
		if ((power & 1n) === 1n) {
			result = (result * factor) % modulus;
		}
		factor = (factor * factor) % modulus;
		power >>= 1n;
	}
	return result;
}

function readRsaPublicKeyFromSequence(reader: DerReader): BrowserRsaKey {
	const modulusBytes = reader.readInteger();
	const exponentBytes = reader.readInteger();
	return {
		modulus: bytesToBigInt(modulusBytes),
		exponent: bytesToBigInt(exponentBytes),
		modulusLength: modulusBytes.byteLength,
	};
}

function parseRsaPublicKey(key: unknown): BrowserRsaKey {
	const der = pemToDerBytes(key);
	const outer = new DerReader(der).readSequence();
	outer.skipAny();
	const publicKey = new DerReader(outer.readBitString()).readSequence();
	return readRsaPublicKeyFromSequence(publicKey);
}

function parseRsaPrivateKey(key: unknown): BrowserRsaKey {
	const der = pemToDerBytes(key);
	const outer = new DerReader(der).readSequence();
	outer.skipAny();
	outer.skipAny();
	const privateKey = new DerReader(outer.readOctetString()).readSequence();
	privateKey.skipAny();
	const modulusBytes = privateKey.readInteger();
	privateKey.skipAny();
	const privateExponentBytes = privateKey.readInteger();
	return {
		modulus: bytesToBigInt(modulusBytes),
		exponent: bytesToBigInt(privateExponentBytes),
		modulusLength: modulusBytes.byteLength,
	};
}

function rsaPkcs1DigestInfo(
	algorithm: unknown,
	data: unknown,
): { hashName: BrowserCryptoHashName; encoded: Uint8Array } {
	const hashName = normalizeSignatureHashName(algorithm);
	const digest = hashDigestBytes(hashName, data);
	return {
		hashName,
		encoded: concatBytes([
			hexToBytes(RSA_PKCS1_DIGEST_PREFIXES[hashName]),
			digest,
		]),
	};
}

function rsaPkcs1EncodeDigestInfo(
	hashName: BrowserCryptoHashName,
	digestInfo: Uint8Array,
	length: number,
): Uint8Array {
	const minimumLength = digestInfo.byteLength + 11;
	if (length < minimumLength) {
		throw new Error(`RSA key is too small for ${hashName} signature`);
	}
	const out = new Uint8Array(length);
	out[0] = 0;
	out[1] = 1;
	out.fill(0xff, 2, length - digestInfo.byteLength - 1);
	out[length - digestInfo.byteLength - 1] = 0;
	out.set(digestInfo, length - digestInfo.byteLength);
	return out;
}

function browserRsaSign(
	algorithm: unknown,
	data: unknown,
	key: unknown,
): Uint8Array {
	const privateKey = parseRsaPrivateKey(key);
	const { hashName, encoded } = rsaPkcs1DigestInfo(algorithm, data);
	const padded = rsaPkcs1EncodeDigestInfo(
		hashName,
		encoded,
		privateKey.modulusLength,
	);
	const signature = modPow(
		bytesToBigInt(padded),
		privateKey.exponent,
		privateKey.modulus,
	);
	return bigIntToBytes(signature, privateKey.modulusLength);
}

function browserRsaVerify(
	algorithm: unknown,
	data: unknown,
	key: unknown,
	signatureValue: unknown,
): boolean {
	const publicKey = parseRsaPublicKey(key);
	const signature = toUint8Array(signatureValue);
	if (signature.byteLength !== publicKey.modulusLength) {
		return false;
	}
	const { hashName, encoded } = rsaPkcs1DigestInfo(algorithm, data);
	const expected = rsaPkcs1EncodeDigestInfo(
		hashName,
		encoded,
		publicKey.modulusLength,
	);
	const actual = bigIntToBytes(
		modPow(bytesToBigInt(signature), publicKey.exponent, publicKey.modulus),
		publicKey.modulusLength,
	);
	if (actual.byteLength !== expected.byteLength) {
		return false;
	}
	let diff = 0;
	for (let index = 0; index < expected.byteLength; index++) {
		diff |= actual[index] ^ expected[index];
	}
	return diff === 0;
}

const BROWSER_RSA_PKCS1_PADDING = 1;
const BROWSER_RSA_PKCS1_OAEP_PADDING = 4;

type BrowserRsaAsymmetricOptions = {
	padding?: unknown;
	oaepHash?: unknown;
	oaepLabel?: unknown;
};

function normalizeRsaAsymmetricOptions(
	value: unknown,
): BrowserRsaAsymmetricOptions {
	if (value == null) return {};
	if (typeof value === "string") {
		const parsed = JSON.parse(value);
		return parsed && typeof parsed === "object"
			? (parsed as BrowserRsaAsymmetricOptions)
			: {};
	}
	return typeof value === "object"
		? (value as BrowserRsaAsymmetricOptions)
		: {};
}

function randomNonZeroBytes(length: number): Uint8Array {
	const bytes = new Uint8Array(length);
	const crypto = globalThis.crypto;
	if (!crypto?.getRandomValues) {
		throw new Error("Browser runtime crypto requires getRandomValues support");
	}
	let offset = 0;
	while (offset < length) {
		const candidate = new Uint8Array(length - offset);
		crypto.getRandomValues(candidate);
		for (const byte of candidate) {
			if (byte !== 0) {
				bytes[offset++] = byte;
				if (offset === length) break;
			}
		}
	}
	return bytes;
}

function xorBytes(left: Uint8Array, right: Uint8Array): Uint8Array {
	if (left.byteLength !== right.byteLength) {
		throw new Error("RSA OAEP mask length mismatch");
	}
	const out = new Uint8Array(left.byteLength);
	for (let index = 0; index < out.byteLength; index++) {
		out[index] = left[index] ^ right[index];
	}
	return out;
}

function mgf1(
	seed: Uint8Array,
	length: number,
	hashName: BrowserCryptoHashName,
): Uint8Array {
	const chunks: Uint8Array[] = [];
	let counter = 0;
	let remaining = length;
	while (remaining > 0) {
		const counterBytes = new Uint8Array([
			(counter >>> 24) & 0xff,
			(counter >>> 16) & 0xff,
			(counter >>> 8) & 0xff,
			counter & 0xff,
		]);
		const digest = hashDigestBytes(hashName, concatBytes([seed, counterBytes]));
		chunks.push(digest.subarray(0, Math.min(remaining, digest.byteLength)));
		remaining -= digest.byteLength;
		counter++;
	}
	return concatBytes(chunks).subarray(0, length);
}

function normalizeOaepHashName(value: unknown): BrowserCryptoHashName {
	return value == null ? "sha1" : normalizeCryptoHashName(value);
}

function rsaEncryptPkcs1(key: BrowserRsaKey, data: Uint8Array): Uint8Array {
	const paddingLength = key.modulusLength - data.byteLength - 3;
	if (paddingLength < 8) {
		throw new Error("RSA_PKCS1_PADDING input is too large for key size");
	}
	const encoded = concatBytes([
		new Uint8Array([0, 2]),
		randomNonZeroBytes(paddingLength),
		new Uint8Array([0]),
		data,
	]);
	const encrypted = modPow(bytesToBigInt(encoded), key.exponent, key.modulus);
	return bigIntToBytes(encrypted, key.modulusLength);
}

function rsaDecryptPkcs1(
	key: BrowserRsaKey,
	encrypted: Uint8Array,
): Uint8Array {
	if (encrypted.byteLength !== key.modulusLength) {
		throw new Error("RSA_PKCS1_PADDING encrypted input has invalid length");
	}
	const encoded = bigIntToBytes(
		modPow(bytesToBigInt(encrypted), key.exponent, key.modulus),
		key.modulusLength,
	);
	if (encoded[0] !== 0 || encoded[1] !== 2) {
		throw new Error("RSA_PKCS1_PADDING block is invalid");
	}
	let separator = -1;
	for (let index = 2; index < encoded.byteLength; index++) {
		if (encoded[index] === 0) {
			separator = index;
			break;
		}
	}
	if (separator < 10) {
		throw new Error("RSA_PKCS1_PADDING block is invalid");
	}
	return encoded.subarray(separator + 1);
}

function rsaEncryptOaep(
	key: BrowserRsaKey,
	data: Uint8Array,
	options: BrowserRsaAsymmetricOptions,
): Uint8Array {
	const hashName = normalizeOaepHashName(options.oaepHash);
	const label = optionalBytes(options.oaepLabel);
	const hash = browserCryptoHash(hashName);
	const hLen = hash.outputLen;
	const maxMessageLength = key.modulusLength - 2 * hLen - 2;
	if (data.byteLength > maxMessageLength) {
		throw new Error("RSA_PKCS1_OAEP_PADDING input is too large for key size");
	}
	const lHash = hash(label);
	const ps = new Uint8Array(maxMessageLength - data.byteLength);
	const db = concatBytes([lHash, ps, new Uint8Array([1]), data]);
	const seed = new Uint8Array(hLen);
	if (!globalThis.crypto?.getRandomValues) {
		throw new Error("Browser runtime crypto requires getRandomValues support");
	}
	globalThis.crypto.getRandomValues(seed);
	const dbMask = mgf1(seed, key.modulusLength - hLen - 1, hashName);
	const maskedDb = xorBytes(db, dbMask);
	const seedMask = mgf1(maskedDb, hLen, hashName);
	const maskedSeed = xorBytes(seed, seedMask);
	const encoded = concatBytes([new Uint8Array([0]), maskedSeed, maskedDb]);
	const encrypted = modPow(bytesToBigInt(encoded), key.exponent, key.modulus);
	return bigIntToBytes(encrypted, key.modulusLength);
}

function rsaDecryptOaep(
	key: BrowserRsaKey,
	encrypted: Uint8Array,
	options: BrowserRsaAsymmetricOptions,
): Uint8Array {
	if (encrypted.byteLength !== key.modulusLength) {
		throw new Error(
			"RSA_PKCS1_OAEP_PADDING encrypted input has invalid length",
		);
	}
	const hashName = normalizeOaepHashName(options.oaepHash);
	const label = optionalBytes(options.oaepLabel);
	const hash = browserCryptoHash(hashName);
	const hLen = hash.outputLen;
	if (key.modulusLength < 2 * hLen + 2) {
		throw new Error("RSA key is too small for OAEP");
	}
	const encoded = bigIntToBytes(
		modPow(bytesToBigInt(encrypted), key.exponent, key.modulus),
		key.modulusLength,
	);
	const maskedSeed = encoded.subarray(1, 1 + hLen);
	const maskedDb = encoded.subarray(1 + hLen);
	const seedMask = mgf1(maskedDb, hLen, hashName);
	const seed = xorBytes(maskedSeed, seedMask);
	const dbMask = mgf1(seed, key.modulusLength - hLen - 1, hashName);
	const db = xorBytes(maskedDb, dbMask);
	const lHash = hash(label);
	let diff = encoded[0];
	for (let index = 0; index < hLen; index++) {
		diff |= db[index] ^ lHash[index];
	}
	let separator = -1;
	for (let index = hLen; index < db.byteLength; index++) {
		if (separator === -1 && db[index] === 1) {
			separator = index;
			break;
		}
		if (db[index] !== 0) {
			diff |= db[index];
		}
	}
	if (diff !== 0 || separator === -1) {
		throw new Error("RSA_PKCS1_OAEP_PADDING block is invalid");
	}
	return db.subarray(separator + 1);
}

function browserRsaAsymmetricOp(
	operation: unknown,
	keyValue: unknown,
	dataValue: unknown,
	optionsValue?: unknown,
): Uint8Array {
	const operationName = String(operation);
	const options = normalizeRsaAsymmetricOptions(optionsValue);
	const padding =
		options.padding == null
			? BROWSER_RSA_PKCS1_OAEP_PADDING
			: Number(options.padding);
	const data = toUint8Array(dataValue);
	if (operationName === "publicEncrypt") {
		const publicKey = parseRsaPublicKey(keyValue);
		if (padding === BROWSER_RSA_PKCS1_PADDING) {
			return rsaEncryptPkcs1(publicKey, data);
		}
		if (padding === BROWSER_RSA_PKCS1_OAEP_PADDING) {
			return rsaEncryptOaep(publicKey, data, options);
		}
	}
	if (operationName === "privateDecrypt") {
		const privateKey = parseRsaPrivateKey(keyValue);
		if (padding === BROWSER_RSA_PKCS1_PADDING) {
			return rsaDecryptPkcs1(privateKey, data);
		}
		if (padding === BROWSER_RSA_PKCS1_OAEP_PADDING) {
			return rsaDecryptOaep(privateKey, data, options);
		}
	}
	throw new Error(
		`Unsupported browser RSA asymmetric operation: ${operationName}`,
	);
}

function pbkdf2Bytes(
	password: unknown,
	salt: unknown,
	iterations: unknown,
	keyLength: unknown,
	algorithm: unknown,
): Uint8Array {
	const normalizedIterations = Number(iterations);
	if (!Number.isInteger(normalizedIterations) || normalizedIterations <= 0) {
		throw new Error("crypto.pbkdf2 iterations must be greater than zero");
	}
	const length = Number(keyLength);
	if (!Number.isInteger(length) || length < 0) {
		throw new Error("crypto.pbkdf2 key length must be a non-negative integer");
	}
	return noblePbkdf2(
		browserCryptoHash(algorithm),
		toUint8Array(password),
		toUint8Array(salt),
		{ c: normalizedIterations, dkLen: length },
	);
}

type BrowserAesMode = "cbc" | "ctr" | "gcm";

type BrowserCipherivOptions = {
	authTag?: unknown;
	autoPadding?: unknown;
	aad?: unknown;
};

function normalizeBrowserAesAlgorithm(algorithm: unknown): {
	mode: BrowserAesMode;
	keyLength: number;
} {
	const normalized = String(algorithm).toLowerCase();
	const match = /^aes-(128|192|256)-(cbc|ctr|gcm)$/.exec(normalized);
	if (!match) {
		throw new Error(`Unsupported browser crypto cipher: ${algorithm}`);
	}
	return {
		keyLength: Number(match[1]) / 8,
		mode: match[2] as BrowserAesMode,
	};
}

function normalizeBrowserCipherivOptions(
	optionsJson: unknown,
): BrowserCipherivOptions {
	if (optionsJson == null) return {};
	if (typeof optionsJson === "string") {
		const parsed = JSON.parse(optionsJson);
		return parsed && typeof parsed === "object"
			? (parsed as BrowserCipherivOptions)
			: {};
	}
	return typeof optionsJson === "object"
		? (optionsJson as BrowserCipherivOptions)
		: {};
}

function optionalBytes(value: unknown): Uint8Array {
	return value == null ? new Uint8Array(0) : toUint8Array(value);
}

function assertAesInputLengths(
	algorithm: unknown,
	key: Uint8Array,
	iv: Uint8Array,
	mode: BrowserAesMode,
	keyLength: number,
): void {
	if (key.byteLength !== keyLength) {
		throw new Error(
			`Invalid key length for ${String(algorithm)}: expected ${keyLength} bytes, got ${key.byteLength}`,
		);
	}
	if ((mode === "cbc" || mode === "ctr") && iv.byteLength !== 16) {
		throw new Error(
			`Invalid IV length for ${String(algorithm)}: expected 16 bytes, got ${iv.byteLength}`,
		);
	}
	if (mode === "gcm" && iv.byteLength < 8) {
		throw new Error(
			`Invalid IV length for ${String(algorithm)}: expected at least 8 bytes, got ${iv.byteLength}`,
		);
	}
}

function browserCipheriv(
	algorithm: unknown,
	keyValue: unknown,
	ivValue: unknown,
	dataValue: unknown,
	optionsJson?: unknown,
): Uint8Array {
	const { mode, keyLength } = normalizeBrowserAesAlgorithm(algorithm);
	const key = toUint8Array(keyValue);
	const iv = toUint8Array(ivValue);
	const data = toUint8Array(dataValue);
	const options = normalizeBrowserCipherivOptions(optionsJson);
	assertAesInputLengths(algorithm, key, iv, mode, keyLength);
	if (mode === "cbc") {
		return nobleAesCbc(key, iv, {
			disablePadding: options.autoPadding === false,
		}).encrypt(data);
	}
	if (mode === "ctr") {
		return nobleAesCtr(key, iv).encrypt(data);
	}
	const encrypted = nobleAesGcm(key, iv, optionalBytes(options.aad)).encrypt(
		data,
	);
	const tagLength = nobleAesGcm.tagLength;
	const ciphertextLength = encrypted.byteLength - tagLength;
	const out = new Uint8Array(encrypted.byteLength);
	out.set(encrypted.subarray(0, ciphertextLength), 0);
	out.set(encrypted.subarray(ciphertextLength), ciphertextLength);
	return out;
}

function browserDecipheriv(
	algorithm: unknown,
	keyValue: unknown,
	ivValue: unknown,
	dataValue: unknown,
	optionsJson?: unknown,
): Uint8Array {
	const { mode, keyLength } = normalizeBrowserAesAlgorithm(algorithm);
	const key = toUint8Array(keyValue);
	const iv = toUint8Array(ivValue);
	const data = toUint8Array(dataValue);
	const options = normalizeBrowserCipherivOptions(optionsJson);
	assertAesInputLengths(algorithm, key, iv, mode, keyLength);
	if (mode === "cbc") {
		return nobleAesCbc(key, iv, {
			disablePadding: options.autoPadding === false,
		}).decrypt(data);
	}
	if (mode === "ctr") {
		return nobleAesCtr(key, iv).decrypt(data);
	}
	const authTag = optionalBytes(options.authTag);
	if (authTag.byteLength === 0) {
		throw new Error(`Missing auth tag for ${String(algorithm)} decipher`);
	}
	const combined = new Uint8Array(data.byteLength + authTag.byteLength);
	combined.set(data, 0);
	combined.set(authTag, data.byteLength);
	return nobleAesGcm(key, iv, optionalBytes(options.aad)).decrypt(combined);
}

function unsupportedBrowserCrypto(operation: string): never {
	const error = new Error(
		`ERR_UNSUPPORTED_BROWSER_CRYPTO: node:crypto ${operation} is not implemented in the browser runtime yet`,
	);
	(error as { code?: string }).code = "ERR_UNSUPPORTED_BROWSER_CRYPTO";
	throw error;
}

function boundErrorMessage(message: string): string {
	if (message.length <= MAX_ERROR_MESSAGE_CHARS) {
		return message;
	}
	return `${message.slice(0, MAX_ERROR_MESSAGE_CHARS)}...[Truncated]`;
}

function boundStdioMessage(message: string): string {
	if (message.length <= MAX_STDIO_MESSAGE_CHARS) {
		return message;
	}
	return `${message.slice(0, MAX_STDIO_MESSAGE_CHARS)}...[Truncated]`;
}

/**
 * Wrap a sync function in the bridge calling convention (`applySync`) so
 * bridge code can call it the same way it calls bridge References.
 */
function makeApplySync<TArgs extends unknown[], TResult>(
	fn: (...args: TArgs) => TResult,
) {
	const applySync = (_ctx: undefined, args: TArgs): TResult => fn(...args);
	return {
		applySync,
		applySyncPromise: applySync,
	};
}

function makeApplySyncPromise<TArgs extends unknown[], TResult>(
	fn: (...args: TArgs) => Promise<TResult>,
) {
	return {
		applySyncPromise(_ctx: undefined, args: TArgs): Promise<TResult> {
			return fn(...args);
		},
	};
}

function makeApplyPromise<TArgs extends unknown[], TResult>(
	fn: (...args: TArgs) => Promise<TResult>,
) {
	return {
		apply(_ctx: undefined, args: TArgs): Promise<TResult> {
			return fn(...args);
		},
	};
}

function normalizeTextEncoding(options?: unknown): BufferEncoding | null {
	if (typeof options === "string") {
		return options as BufferEncoding;
	}

	if (options && typeof options === "object" && "encoding" in options) {
		const encoding = (options as { encoding?: unknown }).encoding;
		return typeof encoding === "string" ? (encoding as BufferEncoding) : null;
	}

	return null;
}

function toNodeBuffer(bytes: Uint8Array): Uint8Array | Buffer {
	if (typeof Buffer === "function") {
		return Buffer.from(bytes);
	}
	return bytes;
}

function createStats(stat: VirtualStat) {
	return {
		...stat,
		isFile: () => !stat.isDirectory && !stat.isSymbolicLink,
		isDirectory: () => stat.isDirectory,
		isSymbolicLink: () => stat.isSymbolicLink,
	};
}

function createDirent(entry: VirtualDirEntry) {
	return {
		name: entry.name,
		isFile: () => !entry.isDirectory && !entry.isSymbolicLink,
		isDirectory: () => entry.isDirectory,
		isSymbolicLink: () => Boolean(entry.isSymbolicLink),
	};
}

function createFsModule(syncBridge: ReturnType<typeof createSyncBridgeClient>) {
	// fd-based fs ops for kernel-backed callers (the shared WASI preview1 runner
	// opens files and reads/writes by descriptor). The browser kernel/wire only
	// exposes path-based ops plus positional `pread`, so descriptors are a JS-side
	// handle table over those permission-checked kernel ops: reads map to `pread`,
	// writes to a read-modify-write of the whole file via `write_file`. The caller
	// (the runner) tracks the per-fd offset and passes an explicit position, so the
	// table itself only needs the path. Enforcement stays in the kernel (every
	// pread / read_file / write_file is permission-checked = S3).
	const openFileTable = new Map<number, { path: string; flags: number }>();
	let nextOpenFd = 1000;
	const makeFsError = (code: string, syscall: string, path: string) => {
		const error = new Error(`${code}: ${syscall} '${path}'`);
		(error as { code?: string }).code = code;
		const errno = posixErrno(code);
		if (errno !== undefined) {
			(error as { errno?: number }).errno = errno;
		}
		(error as { syscall?: string }).syscall = syscall;
		return error;
	};
	const requireOpenFd = (fd: unknown, syscall: string) => {
		const entry = openFileTable.get(Number(fd));
		if (!entry) {
			throw makeFsError("EBADF", syscall, String(fd));
		}
		return entry;
	};
	const O_RDONLY = 0;
	const O_WRONLY = 1;
	const O_RDWR = 2;
	const O_CREAT = 64;
	const O_EXCL = 128;
	const O_TRUNC = 512;
	const O_APPEND = 1024;
	// Node's string open-flag vocabulary -> POSIX flag bits. The browser fd-table
	// otherwise treated any string mode as O_RDONLY (`'w'`/`'a'`/`'r+'` silently
	// became read-only: no create, no truncate, no append), diverging hard from
	// native `fs.openSync`. Sync variants (`'rs'`, `'as'`) map to their non-sync
	// bits since the in-memory VFS has nothing to bypass-cache.
	const STRING_OPEN_FLAGS: Record<string, number> = {
		r: O_RDONLY,
		rs: O_RDONLY,
		sr: O_RDONLY,
		"r+": O_RDWR,
		"rs+": O_RDWR,
		"sr+": O_RDWR,
		w: O_WRONLY | O_CREAT | O_TRUNC,
		wx: O_WRONLY | O_CREAT | O_TRUNC | O_EXCL,
		xw: O_WRONLY | O_CREAT | O_TRUNC | O_EXCL,
		"w+": O_RDWR | O_CREAT | O_TRUNC,
		"wx+": O_RDWR | O_CREAT | O_TRUNC | O_EXCL,
		"xw+": O_RDWR | O_CREAT | O_TRUNC | O_EXCL,
		a: O_WRONLY | O_CREAT | O_APPEND,
		ax: O_WRONLY | O_CREAT | O_APPEND | O_EXCL,
		xa: O_WRONLY | O_CREAT | O_APPEND | O_EXCL,
		as: O_WRONLY | O_CREAT | O_APPEND,
		sa: O_WRONLY | O_CREAT | O_APPEND,
		"a+": O_RDWR | O_CREAT | O_APPEND,
		"ax+": O_RDWR | O_CREAT | O_APPEND | O_EXCL,
		"xa+": O_RDWR | O_CREAT | O_APPEND | O_EXCL,
		"as+": O_RDWR | O_CREAT | O_APPEND,
		"sa+": O_RDWR | O_CREAT | O_APPEND,
	};
	const normalizeOpenFlags = (flags: number | string): number => {
		if (typeof flags === "number") {
			return flags;
		}
		const mapped = STRING_OPEN_FLAGS[flags];
		if (mapped === undefined) {
			const error = new Error(`Unknown file open flag: ${flags}`);
			(error as { code?: string }).code = "ERR_INVALID_ARG_VALUE";
			throw error;
		}
		return mapped;
	};
	const preadBinary = (path: string, offset: number, length: number) => {
		if (length <= 0) {
			return new Uint8Array(0);
		}
		return toUint8Array(
			syncBridge.requestBinary("fs.pread", [path, offset, length]),
		);
	};

	const readFileSync = (path: string, options?: unknown) => {
		const encoding = normalizeTextEncoding(options);
		if (encoding) {
			return syncBridge.requestText("fs.readFile", [path]);
		}
		return toNodeBuffer(syncBridge.requestBinary("fs.readFileBinary", [path]));
	};

	const writeFileSync = (path: string, content: unknown) => {
		if (typeof content === "string") {
			syncBridge.requestVoid("fs.writeFile", [path, content]);
			return;
		}

		syncBridge.requestVoid("fs.writeFileBinary", [path, toUint8Array(content)]);
	};

	const mkdirSync = (
		path: string,
		options?: { recursive?: boolean } | boolean,
	) => {
		const recursive =
			typeof options === "boolean" ? options : (options?.recursive ?? true);
		if (recursive) {
			syncBridge.requestVoid("fs.mkdir", [path]);
			return;
		}
		syncBridge.requestVoid("fs.createDir", [path]);
	};

	const readdirSync = (path: string, options?: { withFileTypes?: boolean }) => {
		const entries = syncBridge.requestJson<VirtualDirEntry[]>("fs.readDir", [
			path,
		]);
		if (options?.withFileTypes) {
			return entries.map((entry) => createDirent(entry));
		}
		return entries.map((entry) => entry.name);
	};

	const statSync = (path: string) =>
		createStats(syncBridge.requestJson<VirtualStat>("fs.stat", [path]));
	const lstatSync = (path: string) =>
		createStats(syncBridge.requestJson<VirtualStat>("fs.lstat", [path]));

	const promises = {
		readFile(path: string, options?: unknown) {
			return Promise.resolve(readFileSync(path, options));
		},
		writeFile(path: string, content: unknown) {
			writeFileSync(path, content);
			return Promise.resolve();
		},
		mkdir(path: string, options?: { recursive?: boolean } | boolean) {
			mkdirSync(path, options);
			return Promise.resolve();
		},
		readdir(path: string, options?: { withFileTypes?: boolean }) {
			return Promise.resolve(readdirSync(path, options));
		},
		stat(path: string) {
			return Promise.resolve(statSync(path));
		},
		lstat(path: string) {
			return Promise.resolve(lstatSync(path));
		},
		unlink(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
			return Promise.resolve();
		},
		rmdir(path: string) {
			syncBridge.requestVoid("fs.rmdir", [path]);
			return Promise.resolve();
		},
		rm(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
			return Promise.resolve();
		},
		rename(oldPath: string, newPath: string) {
			syncBridge.requestVoid("fs.rename", [oldPath, newPath]);
			return Promise.resolve();
		},
		realpath(path: string) {
			return Promise.resolve(syncBridge.requestText("fs.realpath", [path]));
		},
		readlink(path: string) {
			return Promise.resolve(syncBridge.requestText("fs.readlink", [path]));
		},
		symlink(target: string, path: string) {
			syncBridge.requestVoid("fs.symlink", [target, path]);
			return Promise.resolve();
		},
		link(existingPath: string, newPath: string) {
			syncBridge.requestVoid("fs.link", [existingPath, newPath]);
			return Promise.resolve();
		},
		chmod(path: string, mode: number) {
			syncBridge.requestVoid("fs.chmod", [path, mode]);
			return Promise.resolve();
		},
		truncate(path: string, length = 0) {
			syncBridge.requestVoid("fs.truncate", [path, length]);
			return Promise.resolve();
		},
	};

	return {
		readFileSync,
		writeFileSync,
		mkdirSync,
		readdirSync,
		existsSync(path: string) {
			return syncBridge.requestJson<boolean>("fs.exists", [path]);
		},
		statSync,
		lstatSync,
		unlinkSync(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
		},
		rmdirSync(path: string) {
			syncBridge.requestVoid("fs.rmdir", [path]);
		},
		rmSync(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
		},
		renameSync(oldPath: string, newPath: string) {
			syncBridge.requestVoid("fs.rename", [oldPath, newPath]);
		},
		realpathSync(path: string) {
			return syncBridge.requestText("fs.realpath", [path]);
		},
		readlinkSync(path: string) {
			return syncBridge.requestText("fs.readlink", [path]);
		},
		symlinkSync(target: string, path: string) {
			syncBridge.requestVoid("fs.symlink", [target, path]);
		},
		linkSync(existingPath: string, newPath: string) {
			syncBridge.requestVoid("fs.link", [existingPath, newPath]);
		},
		chmodSync(path: string, mode: number) {
			syncBridge.requestVoid("fs.chmod", [path, mode]);
		},
		truncateSync(path: string, length = 0) {
			syncBridge.requestVoid("fs.truncate", [path, length]);
		},
		constants: {
			O_RDONLY: 0,
			O_WRONLY: 1,
			O_RDWR: 2,
			O_CREAT: 64,
			O_EXCL: 128,
			O_TRUNC: 512,
			O_APPEND: 1024,
			O_DIRECTORY: 65536,
		},
		openSync(path: string, flags: number | string = 0) {
			const f = normalizeOpenFlags(flags);
			if ((f & O_CREAT) !== 0) {
				const exists = syncBridge.requestJson<boolean>("fs.exists", [path]);
				if (exists && (f & O_EXCL) !== 0) {
					throw makeFsError("EEXIST", "open", path);
				}
				if (!exists) {
					syncBridge.requestVoid("fs.writeFile", [path, ""]);
				}
			}
			if ((f & O_TRUNC) !== 0) {
				syncBridge.requestVoid("fs.truncate", [path, 0]);
			}
			// Linux checks read permission at open time, but the kernel/wire only
			// enforces it on the actual read (pread). For a read-capable open (not
			// write-only, not freshly created/truncated empty), probe one byte so a
			// denied read surfaces as EACCES at open, matching open(O_RDONLY)
			// semantics. The probe is positional (no fd offset effect).
			const accmode = f & 3;
			const readCapable = accmode === 0 || accmode === 2;
			if (readCapable && (f & (O_CREAT | O_TRUNC)) === 0) {
				try {
					preadBinary(path, 0, 1);
				} catch (error) {
					// Only a permission denial should fail the open; other probe
					// errors (e.g. EISDIR on a directory open) are left for the real
					// open/read path to surface.
					const code = (error as { code?: string })?.code;
					if (code === "EACCES" || code === "EPERM") {
						throw error;
					}
				}
			}
			const fd = nextOpenFd++;
			openFileTable.set(fd, { path, flags: f });
			return fd;
		},
		readSync(
			fd: number,
			buffer: Uint8Array,
			offset = 0,
			length?: number,
			position?: number | null,
		) {
			const entry = requireOpenFd(fd, "read");
			const len = typeof length === "number" ? length : buffer.length - offset;
			const pos = typeof position === "number" && position >= 0 ? position : 0;
			const data = preadBinary(entry.path, pos, len);
			const n = Math.min(data.length, len);
			for (let i = 0; i < n; i += 1) {
				buffer[offset + i] = data[i];
			}
			return n;
		},
		writeSync(
			fd: number,
			buffer: Uint8Array,
			offset = 0,
			length?: number,
			position?: number | null,
		) {
			const entry = requireOpenFd(fd, "write");
			const src = toUint8Array(buffer);
			const len = typeof length === "number" ? length : src.length - offset;
			const chunk = src.subarray(offset, offset + len);
			// Resolve the write position. POSIX O_APPEND atomically writes at the
			// current end of file regardless of any tracked offset, so honor it
			// when the caller did not pin an explicit position (the WASI runner
			// always passes one and tracks its own offset, so it is unaffected).
			let pos: number;
			if (typeof position === "number" && position >= 0) {
				pos = position;
			} else if ((entry.flags & O_APPEND) !== 0) {
				pos = statSync(entry.path).size;
			} else {
				pos = 0;
			}
			// Positional write straight through the kernel: a single permission-
			// checked, atomic `pwrite` that grows and zero-fills as needed. The
			// previous client-side read-modify-write silently discarded the whole
			// file whenever the readback failed (permission/read-cap/IO) and was
			// O(filesize) and non-atomic across concurrent fds.
			syncBridge.requestVoid("fs.pwrite", [entry.path, pos, chunk]);
			return len;
		},
		closeSync(fd: number) {
			openFileTable.delete(Number(fd));
		},
		fstatSync(fd: number) {
			return statSync(requireOpenFd(fd, "fstat").path);
		},
		fchmodSync(fd: number, mode: number) {
			syncBridge.requestVoid("fs.chmod", [
				requireOpenFd(fd, "fchmod").path,
				Number(mode) || 0,
			]);
		},
		ftruncateSync(fd: number, length = 0) {
			syncBridge.requestVoid("fs.truncate", [
				requireOpenFd(fd, "ftruncate").path,
				Number(length) || 0,
			]);
		},
		fsyncSync() {},
		fdatasyncSync() {},
		promises,
	};
}

// Save real postMessage before sandbox code can replace it
const _realPostMessage = self.postMessage.bind(self);

function postResponse(
	message:
		| {
				type: "response";
				id: number;
				ok: true;
				result: ExecResult | RunResult | true;
		  }
		| {
				type: "response";
				id: number;
				ok: false;
				error: { message: string; stack?: string; code?: string };
		  },
): void {
	_realPostMessage({
		controlToken: getRequiredControlToken(),
		...message,
	} satisfies BrowserWorkerOutboundMessage);
}

function postAsyncResponse<T extends ExecResult | RunResult>(
	id: number,
	promise: Promise<T>,
): void {
	void promise.then(
		(result) => {
			postResponse({ type: "response", id, ok: true, result });
		},
		(err) => {
			const error = err as { message?: string; stack?: string; code?: string };
			postResponse({
				type: "response",
				id,
				ok: false,
				error: {
					message: error?.message ?? String(err),
					stack: error?.stack,
					code: error?.code,
				},
			});
		},
	);
}

function postSyncRequest(message: {
	type: "sync-request";
	requestId: number;
	executionId: string;
	processRequestId: number;
	operation: BrowserWorkerSyncOperation;
	args: unknown[];
}): void {
	_realPostMessage({
		controlToken: getRequiredControlToken(),
		...message,
	} satisfies BrowserWorkerOutboundMessage);
}

function postStdio(
	executionId: string,
	requestId: number,
	channel: StdioChannel,
	message: string,
): void {
	const payload: BrowserWorkerOutboundMessage = {
		controlToken: getRequiredControlToken(),
		type: "stdio",
		executionId,
		requestId,
		channel,
		message,
	};
	_realPostMessage(payload);
}

function postPtyOpened(
	executionId: string,
	requestId: number,
	pty: {
		masterFd: number;
		slaveFd: number;
		path?: string;
		columns: number;
		rows: number;
	},
): void {
	const payload: BrowserWorkerOutboundMessage = {
		controlToken: getRequiredControlToken(),
		type: "pty-opened",
		executionId,
		requestId,
		...pty,
	};
	_realPostMessage(payload);
}

function emitStdio(
	executionId: string,
	requestId: number,
	channel: StdioChannel,
	message: string,
): void {
	postStdio(executionId, requestId, channel, boundStdioMessage(message));
}

function emitActiveStdio(channel: StdioChannel, args: unknown[]): void {
	if (
		!activeCaptureStdio ||
		activeProcessRequestId === null ||
		activeExecutionId === null
	) {
		return;
	}
	const message = args.map((arg) => normalizeProcessOutputChunk(arg)).join(" ");
	emitStdio(activeExecutionId, activeProcessRequestId, channel, message);
}

function createSyncBridgeClient(payload: BrowserSyncBridgePayload) {
	const signal = new Int32Array(payload.signalBuffer);
	const data = new Uint8Array(payload.dataBuffer);
	let nextRequestId = 1;
	const timeoutMs = payload.timeoutMs ?? 30_000;

	function readBytes(length: number): Uint8Array {
		if (length <= 0) {
			return new Uint8Array(0);
		}
		return data.slice(0, length);
	}

	function requestRaw(
		operation: BrowserWorkerSyncOperation,
		args: unknown[],
	): {
		kind: number;
		bytes: Uint8Array;
	} {
		if (!activeExecutionId || activeProcessRequestId === null) {
			throw new Error(
				`Browser runtime sync bridge ${operation} called outside an active execution`,
			);
		}
		Atomics.store(
			signal,
			SYNC_BRIDGE_SIGNAL_STATE_INDEX,
			SYNC_BRIDGE_SIGNAL_STATE_IDLE,
		);
		Atomics.store(signal, SYNC_BRIDGE_SIGNAL_STATUS_INDEX, 0);
		Atomics.store(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX, SYNC_BRIDGE_KIND_NONE);
		Atomics.store(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX, 0);

		postSyncRequest({
			type: "sync-request",
			requestId: nextRequestId++,
			executionId: activeExecutionId,
			processRequestId: activeProcessRequestId,
			operation,
			args,
		});

		while (true) {
			const result = Atomics.wait(
				signal,
				SYNC_BRIDGE_SIGNAL_STATE_INDEX,
				SYNC_BRIDGE_SIGNAL_STATE_IDLE,
				timeoutMs,
			);
			if (result !== "timed-out") {
				break;
			}
			throw new Error(
				`Browser runtime sync bridge timed out while handling ${operation}`,
			);
		}

		const status = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_STATUS_INDEX);
		const kind = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX);
		const length = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX);
		const bytes = readBytes(length);
		Atomics.store(
			signal,
			SYNC_BRIDGE_SIGNAL_STATE_INDEX,
			SYNC_BRIDGE_SIGNAL_STATE_IDLE,
		);

		if (status === SYNC_BRIDGE_STATUS_ERROR) {
			const errorPayload = JSON.parse(
				decoder.decode(bytes),
			) as BrowserSyncBridgeErrorPayload;
			const error = new Error(errorPayload.message);
			if (errorPayload.code) {
				(error as { code?: string }).code = errorPayload.code;
				const errno = posixErrno(errorPayload.code);
				if (errno !== undefined) {
					(error as { errno?: number }).errno = errno;
				}
			}
			throw error;
		}

		return { kind, bytes };
	}

	return {
		requestVoid(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			requestRaw(operation, args);
		},
		requestText(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			const result = requestRaw(operation, args);
			if (result.kind !== SYNC_BRIDGE_KIND_TEXT) {
				throw new Error(
					`Expected text response from ${operation}, received kind ${result.kind}`,
				);
			}
			return decoder.decode(result.bytes);
		},
		requestNullableText(
			operation: BrowserWorkerSyncOperation,
			args: unknown[],
		) {
			const result = requestRaw(operation, args);
			if (result.kind === SYNC_BRIDGE_KIND_NONE) {
				return null;
			}
			if (result.kind !== SYNC_BRIDGE_KIND_TEXT) {
				throw new Error(
					`Expected text response from ${operation}, received kind ${result.kind}`,
				);
			}
			return decoder.decode(result.bytes);
		},
		requestBinary(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			const result = requestRaw(operation, args);
			if (result.kind !== SYNC_BRIDGE_KIND_BINARY) {
				throw new Error(
					`Expected binary response from ${operation}, received kind ${result.kind}`,
				);
			}
			return result.bytes;
		},
		requestJson<T>(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			const result = requestRaw(operation, args);
			if (result.kind !== SYNC_BRIDGE_KIND_JSON) {
				throw new Error(
					`Expected JSON response from ${operation}, received kind ${result.kind}`,
				);
			}
			return JSON.parse(decoder.decode(result.bytes)) as T;
		},
	};
}

/**
 * Initialize the worker-side runtime: set up filesystem, network, bridge
 * globals, and load the bridge bundle. Called once before any exec/run.
 */
async function initRuntime(payload: BrowserWorkerInitPayload): Promise<void> {
	if (initialized) return;
	assertBrowserSyncBridgeSupport();
	captureTimingGlobals();
	if (!payload.syncBridge) {
		throw new Error(
			"Browser runtime sync bridge is required for filesystem and module loading parity",
		);
	}

	const syncBridge = createSyncBridgeClient(payload.syncBridge);
	activeSyncBridge = syncBridge;

	// Apply payload limits (use defaults if not configured)
	base64TransferLimitBytes =
		payload.payloadLimits?.base64TransferBytes ?? DEFAULT_BASE64_TRANSFER_BYTES;
	jsonPayloadLimitBytes =
		payload.payloadLimits?.jsonPayloadBytes ?? DEFAULT_JSON_PAYLOAD_BYTES;

	// Permission policy is enforced solely by the kernel (the trusted sidecar);
	// the guest worker never re-checks it. Net egress for kernel-routed traffic
	// is gated in the kernel socket table; this adapter is the host-network path
	// only present when the embedder injects one.
	if (payload.networkEnabled) {
		networkAdapter = createBrowserNetworkAdapter();
	} else {
		networkAdapter = createNetworkStub();
	}

	const processConfig = payload.processConfig ?? {};
	runtimeProcessConfig = processConfig as Record<string, unknown>;
	runtimeTimingMitigation =
		payload.timingMitigation ??
		processConfig.timingMitigation ??
		runtimeTimingMitigation;
	// env is filtered by the trusted driver before it reaches the worker.
	processConfig.timingMitigation = runtimeTimingMitigation;
	delete processConfig.frozenTimeMs;
	exposeCustomGlobal("_processConfig", processConfig);
	const osConfig = payload.osConfig ?? {};
	exposeCustomGlobal("_osConfig", osConfig);
	exposeCustomGlobal("__agentOSVirtualOs", osConfig);

	exposeCustomGlobal(
		"_log",
		makeApplySync((...args: unknown[]) => {
			emitActiveStdio("stdout", args);
		}),
	);
	exposeCustomGlobal(
		"_error",
		makeApplySync((...args: unknown[]) => {
			emitActiveStdio("stderr", args);
		}),
	);

	// Set up filesystem bridge globals before loading runtime shims.
	const readFileRef = makeApplySync((path: string) => {
		const text = syncBridge.requestText("fs.readFile", [path]);
		assertTextPayloadSize(`fs.readFile ${path}`, text, jsonPayloadLimitBytes);
		return text;
	});
	const writeFileRef = makeApplySync((path: string, content: string) => {
		assertTextPayloadSize(
			`fs.writeFile ${path}`,
			content,
			jsonPayloadLimitBytes,
		);
		syncBridge.requestVoid("fs.writeFile", [path, content]);
	});
	const readFileBinaryRef = makeApplySync((path: string) => {
		const data = syncBridge.requestBinary("fs.readFileBinary", [path]);
		assertPayloadByteLength(
			`fs.readFileBinary ${path}`,
			data.byteLength,
			base64TransferLimitBytes,
		);
		return data;
	});
	const writeFileBinaryRef = makeApplySync(
		(path: string, binaryContent: Uint8Array) => {
			assertPayloadByteLength(
				`fs.writeFileBinary ${path}`,
				binaryContent.byteLength,
				base64TransferLimitBytes,
			);
			syncBridge.requestVoid("fs.writeFileBinary", [path, binaryContent]);
		},
	);
	const readDirRef = makeApplySync((path: string) => {
		const json = JSON.stringify(
			syncBridge.requestJson<VirtualDirEntry[]>("fs.readDir", [path]),
		);
		assertTextPayloadSize(`fs.readDir ${path}`, json, jsonPayloadLimitBytes);
		return json;
	});
	const mkdirRef = makeApplySync((path: string) => {
		syncBridge.requestVoid("fs.mkdir", [path]);
	});
	const rmdirRef = makeApplySync((path: string) => {
		syncBridge.requestVoid("fs.rmdir", [path]);
	});
	const existsRef = makeApplySync((path: string) => {
		return syncBridge.requestJson<boolean>("fs.exists", [path]);
	});
	const statRef = makeApplySync((path: string) => {
		return JSON.stringify(
			syncBridge.requestJson<VirtualStat>("fs.stat", [path]),
		);
	});
	const unlinkRef = makeApplySync((path: string) => {
		syncBridge.requestVoid("fs.unlink", [path]);
	});
	const renameRef = makeApplySync((oldPath: string, newPath: string) => {
		syncBridge.requestVoid("fs.rename", [oldPath, newPath]);
	});

	exposeCustomGlobal("_fs", {
		readFile: readFileRef,
		writeFile: writeFileRef,
		readFileBinary: readFileBinaryRef,
		writeFileBinary: writeFileBinaryRef,
		readDir: readDirRef,
		mkdir: mkdirRef,
		rmdir: rmdirRef,
		exists: existsRef,
		stat: statRef,
		unlink: unlinkRef,
		rename: renameRef,
	});

	exposeCustomGlobal(
		"_loadPolyfill",
		makeApplySync((moduleName: string) => {
			return getRuntimePolyfillCode(moduleName, payload.processLimits);
		}),
	);

	const resolveModuleSync = (
		request: string,
		fromDir: string,
		mode?: "require" | "import",
	) => {
		return syncBridge.requestNullableText("module.resolve", [
			request,
			fromDir,
			mode ?? "require",
		]);
	};
	const loadFileSync = (path: string, _mode?: "require" | "import") => {
		const source = syncBridge.requestNullableText("module.loadFile", [path]);
		if (source === null) {
			return null;
		}
		let code = source;
		if (isESM(source, path)) {
			code = transform(code, { transforms: ["imports"] }).code;
		}
		return transformDynamicImport(code);
	};
	const moduleFormatSync = (path: string) => {
		return syncBridge.requestNullableText("module.format", [path]);
	};
	const batchResolveModulesSync = (requests: Array<[string, string]>) => {
		return syncBridge.requestJson("module.batchResolve", [requests]);
	};

	exposeCustomGlobal("_resolveModuleSync", makeApplySync(resolveModuleSync));
	exposeCustomGlobal("_loadFileSync", makeApplySync(loadFileSync));
	exposeCustomGlobal("_resolveModule", makeApplySync(resolveModuleSync));
	exposeCustomGlobal("_loadFile", makeApplySync(loadFileSync));
	exposeCustomGlobal("_moduleFormat", makeApplySync(moduleFormatSync));
	exposeCustomGlobal(
		"_batchResolveModules",
		makeApplySync(batchResolveModulesSync),
	);

	const randomBytes = (length: number): Uint8Array => {
		if (!Number.isInteger(length) || length < 0) {
			throw new Error(
				"crypto random byte length must be a non-negative integer",
			);
		}
		const bytes = new Uint8Array(length);
		const crypto = globalThis.crypto;
		if (!crypto?.getRandomValues) {
			throw new Error(
				"Browser runtime crypto requires getRandomValues support",
			);
		}
		for (let offset = 0; offset < bytes.length; offset += 65536) {
			crypto.getRandomValues(bytes.subarray(offset, offset + 65536));
		}
		return bytes;
	};
	exposeCustomGlobal(
		"_cryptoRandomFill",
		makeApplySync((length: number) => randomBytes(Number(length))),
	);
	exposeCustomGlobal(
		"_cryptoRandomUUID",
		makeApplySync(() => {
			if (typeof globalThis.crypto?.randomUUID !== "function") {
				throw new Error("Browser runtime crypto requires randomUUID support");
			}
			return globalThis.crypto.randomUUID();
		}),
	);
	exposeCustomGlobal(
		"_cryptoHashDigest",
		makeApplySync((algorithm: string, data: Uint8Array) => {
			return hashDigestBytes(algorithm, data);
		}),
	);
	exposeCustomGlobal(
		"_cryptoHmacDigest",
		makeApplySync((algorithm: string, key: Uint8Array, data: Uint8Array) => {
			return hmacDigestBytes(algorithm, key, data);
		}),
	);
	exposeCustomGlobal(
		"_cryptoPbkdf2",
		makeApplySync(
			(
				password: Uint8Array,
				salt: Uint8Array,
				iterations: number,
				keyLength: number,
				algorithm: string,
			) => {
				return pbkdf2Bytes(password, salt, iterations, keyLength, algorithm);
			},
		),
	);
	exposeCustomGlobal(
		"_cryptoScrypt",
		makeApplySync(
			(
				password: Uint8Array,
				salt: Uint8Array,
				keyLength: number,
				options: unknown,
			) => {
				return nobleScrypt(
					toUint8Array(password),
					toUint8Array(salt),
					normalizeScryptOptions(options, keyLength),
				);
			},
		),
	);
	exposeCustomGlobal(
		"_cryptoCipheriv",
		makeApplySync(
			(
				algorithm: string,
				key: Uint8Array,
				iv: Uint8Array,
				data: Uint8Array,
				optionsJson?: string,
			) => browserCipheriv(algorithm, key, iv, data, optionsJson),
		),
	);
	exposeCustomGlobal(
		"_cryptoDecipheriv",
		makeApplySync(
			(
				algorithm: string,
				key: Uint8Array,
				iv: Uint8Array,
				data: Uint8Array,
				optionsJson?: string,
			) => browserDecipheriv(algorithm, key, iv, data, optionsJson),
		),
	);
	exposeCustomGlobal(
		"_cryptoCipherivCreate",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoCipherivCreate")),
	);
	exposeCustomGlobal(
		"_cryptoCipherivUpdate",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoCipherivUpdate")),
	);
	exposeCustomGlobal(
		"_cryptoCipherivFinal",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoCipherivFinal")),
	);
	exposeCustomGlobal(
		"_cryptoSign",
		makeApplySync((algorithm: string, data: Uint8Array, key: unknown) =>
			browserRsaSign(algorithm, data, key),
		),
	);
	exposeCustomGlobal(
		"_cryptoVerify",
		makeApplySync(
			(
				algorithm: string,
				data: Uint8Array,
				key: unknown,
				signature: Uint8Array,
			) => browserRsaVerify(algorithm, data, key, signature),
		),
	);
	exposeCustomGlobal(
		"_cryptoAsymmetricOp",
		makeApplySync(
			(
				operation: string,
				key: unknown,
				data: Uint8Array,
				optionsJson?: string,
			) => browserRsaAsymmetricOp(operation, key, data, optionsJson),
		),
	);
	exposeCustomGlobal(
		"_cryptoCreateKeyObject",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoCreateKeyObject")),
	);
	exposeCustomGlobal(
		"_cryptoGenerateKeyPairSync",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoGenerateKeyPairSync")),
	);
	exposeCustomGlobal(
		"_cryptoGenerateKeySync",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoGenerateKeySync")),
	);
	exposeCustomGlobal(
		"_cryptoGeneratePrimeSync",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoGeneratePrimeSync")),
	);
	exposeCustomGlobal(
		"_cryptoDiffieHellman",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoDiffieHellman")),
	);
	exposeCustomGlobal(
		"_cryptoDiffieHellmanGroup",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoDiffieHellmanGroup")),
	);
	exposeCustomGlobal(
		"_cryptoDiffieHellmanSessionCreate",
		makeApplySync(() =>
			unsupportedBrowserCrypto("_cryptoDiffieHellmanSessionCreate"),
		),
	);
	exposeCustomGlobal(
		"_cryptoDiffieHellmanSessionCall",
		makeApplySync(() =>
			unsupportedBrowserCrypto("_cryptoDiffieHellmanSessionCall"),
		),
	);
	exposeCustomGlobal(
		"_cryptoDiffieHellmanSessionDestroy",
		makeApplySync(() =>
			unsupportedBrowserCrypto("_cryptoDiffieHellmanSessionDestroy"),
		),
	);
	exposeCustomGlobal(
		"_cryptoSubtle",
		makeApplySync(() => unsupportedBrowserCrypto("_cryptoSubtle")),
	);

	exposeCustomGlobal("_scheduleTimer", {
		apply(_ctx: undefined, args: [number]) {
			return new Promise<void>((resolve) => {
				setTimeout(resolve, args[0]);
			});
		},
	});

	const netAdapter = networkAdapter ?? createNetworkStub();
	const networkFetch = (
		url: string,
		options: {
			method?: string;
			headers?: Record<string, string>;
			body?: string | null;
		},
	) => {
		const result = syncBridge.requestJson("network.fetch", [url, options]);
		return result as Awaited<ReturnType<NetworkAdapter["fetch"]>>;
	};
	exposeCustomGlobal(
		"_networkFetchRaw",
		makeApplyPromise(async (url: string, optionsJson: string) => {
			const options = JSON.parse(optionsJson);
			const result = networkFetch(url, options);
			return JSON.stringify(result);
		}),
	);
	exposeCustomGlobal(
		"_networkDnsLookupRaw",
		makeApplyPromise(async (request: string | { hostname?: unknown }) => {
			const hostname =
				typeof request === "string" ? request : String(request.hostname ?? "");
			const result = await netAdapter.dnsLookup(hostname);
			if (result.error) {
				const error = new Error(result.error);
				(error as { code?: string }).code = result.code;
				throw error;
			}
			return JSON.stringify(result);
		}),
	);

	// Guest global `fetch` over the kernel-brokered network adapter (the same seam
	// `_networkFetchRaw` uses). Real programs (e.g. the pi ACP adapter's LLM SDK) call
	// global fetch to reach their model endpoint; the adapter mediates egress (loopback
	// routes through the kernel). Returns a real WHATWG Response (worker global) so the
	// body is a ReadableStream the caller can stream (e.g. SSE).
	exposeCustomGlobal(
		"fetch",
		async (input: unknown, init?: Record<string, unknown>) => {
			const req = (input ?? {}) as Record<string, unknown>;
			const url = typeof input === "string" ? input : String(req.url ?? input);
			const method = String((init?.method ?? req.method ?? "GET") as string);
			const headers: Record<string, string> = {};
			const rawHeaders = (init?.headers ?? req.headers) as unknown;
			if (rawHeaders) {
				if (Array.isArray(rawHeaders)) {
					for (const entry of rawHeaders as [string, string][])
						headers[entry[0]] = entry[1];
				} else if (
					typeof (rawHeaders as { forEach?: unknown }).forEach === "function"
				) {
					(
						rawHeaders as {
							forEach: (cb: (v: string, k: string) => void) => void;
						}
					).forEach((v, k) => {
						headers[k] = v;
					});
				} else {
					for (const key of Object.keys(
						rawHeaders as Record<string, unknown>,
					)) {
						headers[key] = String((rawHeaders as Record<string, unknown>)[key]);
					}
				}
			}
			let body = (init?.body ?? req.body) as unknown;
			if (body != null && typeof body !== "string") {
				body =
					body instanceof Uint8Array
						? new TextDecoder().decode(body)
						: String(body);
			}
			const result = networkFetch(url, {
				method,
				headers,
				body: (body ?? null) as string | null,
			});
			return new Response((result.body ?? "") as string, {
				status: result.status ?? 200,
				statusText: result.statusText || "",
				headers: (result.headers ?? {}) as Record<string, string>,
			});
		},
	);

	// Node globals guest libraries reference: `global` (the global object) and the
	// immediate timers (macrotask-scheduled; they run under the persistent event loop).
	exposeCustomGlobal("global", globalThis);
	if (
		typeof (globalThis as { setImmediate?: unknown }).setImmediate !==
		"function"
	) {
		exposeCustomGlobal(
			"setImmediate",
			(fn: (...a: unknown[]) => void, ...args: unknown[]) =>
				setTimeout(() => fn(...args), 0),
		);
		exposeCustomGlobal("clearImmediate", (handle: unknown) =>
			clearTimeout(handle as number),
		);
	}

	exposeCustomGlobal(
		"_childProcessSpawnStart",
		makeApplySync((request: BrowserChildProcessSpawnRequest) => {
			return syncBridge.requestJson<number>("child_process.spawn", [request]);
		}),
	);

	exposeCustomGlobal(
		"_childProcessPoll",
		makeApplySync((sessionId: number, _waitMs?: number) => {
			return syncBridge.requestJson<BrowserChildProcessPollEvent | null>(
				"child_process.poll",
				[sessionId, _waitMs ?? 0],
			);
		}),
	);

	exposeCustomGlobal(
		"_childProcessStdinWrite",
		makeApplySync((sessionId: number, data: Uint8Array) => {
			syncBridge.requestVoid("child_process.write_stdin", [sessionId, data]);
		}),
	);

	exposeCustomGlobal(
		"_childProcessStdinClose",
		makeApplySync((sessionId: number) => {
			syncBridge.requestVoid("child_process.close_stdin", [sessionId]);
		}),
	);

	exposeCustomGlobal(
		"_childProcessKill",
		makeApplySync((sessionId: number, signal: number) => {
			return syncBridge.requestJson<boolean>("child_process.kill", [
				sessionId,
				signal,
			]);
		}),
	);

	exposeCustomGlobal(
		"_childProcessPtyResize",
		makeApplySync((sessionId: number, cols: number, rows: number) => {
			syncBridge.requestVoid("child_process.resize_pty", [
				sessionId,
				cols,
				rows,
			]);
		}),
	);

	exposeCustomGlobal(
		"_childProcessSpawnSync",
		makeApplySync((request: BrowserChildProcessSpawnRequest) => {
			return syncBridge.requestText("child_process.spawn_sync", [request]);
		}),
	);
	exposeCustomGlobal(
		"_processSignalState",
		makeApplySync(
			(signal: number, action: string, maskJson: string, flags: number) => {
				syncBridge.requestVoid("process.signal_state", [
					signal,
					action,
					maskJson,
					flags,
				]);
			},
		),
	);
	exposeCustomGlobal(
		"_dgramSocketCreateRaw",
		makeApplySync((options: { type?: unknown }) => {
			return syncBridge.requestJson("dgram.create", [options]);
		}),
	);
	exposeCustomGlobal(
		"_dgramSocketBindRaw",
		makeApplySync((socketId: string | number, options: unknown) => {
			return syncBridge.requestJson("dgram.bind", [socketId, options]);
		}),
	);
	exposeCustomGlobal(
		"_dgramSocketRecvRaw",
		makeApplySync((socketId: string | number, waitMs?: number) => {
			return syncBridge.requestJson("dgram.recv", [socketId, waitMs ?? 0]);
		}),
	);
	exposeCustomGlobal(
		"_dgramSocketSendRaw",
		makeApplySync(
			(socketId: string | number, data: Uint8Array, target: unknown) => {
				return syncBridge.requestJson("dgram.send", [socketId, data, target]);
			},
		),
	);
	exposeCustomGlobal(
		"_dgramSocketCloseRaw",
		makeApplySync((socketId: string | number) => {
			return syncBridge.requestJson("dgram.close", [socketId]);
		}),
	);
	exposeCustomGlobal(
		"_dgramSocketAddressRaw",
		makeApplySync((socketId: string | number) => {
			return syncBridge.requestJson("dgram.address", [socketId]);
		}),
	);
	exposeCustomGlobal(
		"_dgramSocketSetBufferSizeRaw",
		makeApplySync((socketId: string | number, which: string, size: number) => {
			return syncBridge.requestJson("dgram.setBufferSize", [
				socketId,
				which,
				size,
			]);
		}),
	);
	exposeCustomGlobal(
		"_dgramSocketGetBufferSizeRaw",
		makeApplySync((socketId: string | number, which: string) => {
			return syncBridge.requestJson("dgram.getBufferSize", [socketId, which]);
		}),
	);
	exposeCustomGlobal("_fsModule", createFsModule(syncBridge));
	exposeMutableRuntimeStateGlobal("_moduleCache", {});
	exposeMutableRuntimeStateGlobal("_pendingModules", {});
	exposeMutableRuntimeStateGlobal("_currentModule", { dirname: "/" });
	globalEval(getRequireSetupCode());
	ensureProcessGlobal();

	// Block dangerous Web APIs that bypass bridge permission checks
	const dangerousApis = [
		"XMLHttpRequest",
		"WebSocket",
		"importScripts",
		"indexedDB",
		"caches",
		"BroadcastChannel",
	];
	for (const api of dangerousApis) {
		try {
			delete (self as unknown as Record<string, unknown>)[api];
		} catch {
			// May not exist or may be non-configurable
		}
		Object.defineProperty(self, api, {
			get() {
				throw new ReferenceError(`${api} is not available in sandbox`);
			},
			configurable: false,
		});
	}

	// Lock down self.onmessage so sandbox code cannot hijack the control channel
	const currentHandler = self.onmessage;
	Object.defineProperty(self, "onmessage", {
		value: currentHandler,
		writable: false,
		configurable: false,
	});

	// Block self.postMessage so sandbox code cannot forge responses to host
	Object.defineProperty(self, "postMessage", {
		get() {
			throw new TypeError("postMessage is not available in sandbox");
		},
		configurable: false,
	});

	initialized = true;
}

function resetModuleState(cwd: string): void {
	exposeMutableRuntimeStateGlobal("_moduleCache", {});
	exposeMutableRuntimeStateGlobal("_pendingModules", {});
	exposeMutableRuntimeStateGlobal("_currentModule", { dirname: cwd });
}

function setDynamicImportFallback(): void {
	exposeMutableRuntimeStateGlobal("__dynamicImport", (specifier: string) => {
		const cached = dynamicImportCache.get(specifier);
		if (cached) return Promise.resolve(cached);
		try {
			const runtimeRequire = (globalThis as Record<string, unknown>).require as
				| ((request: string) => unknown)
				| undefined;
			if (typeof runtimeRequire !== "function") {
				throw new Error("require is not available in browser runtime");
			}
			const mod = runtimeRequire(specifier);
			return Promise.resolve({
				default: mod,
				...(mod as Record<string, unknown>),
			});
		} catch (e) {
			return Promise.reject(
				new Error(`Cannot dynamically import '${specifier}': ${String(e)}`),
			);
		}
	});
}

function toProcessChunk(
	value: string,
	encoding: string | null,
): string | Uint8Array {
	if (encoding) {
		return value;
	}
	return encoder.encode(value);
}

function normalizeProcessOutputChunk(chunk: unknown): string {
	if (typeof chunk === "string") {
		return chunk;
	}
	if (chunk instanceof Uint8Array) {
		return decoder.decode(chunk);
	}
	if (ArrayBuffer.isView(chunk)) {
		return decoder.decode(
			new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength),
		);
	}
	if (chunk instanceof ArrayBuffer) {
		return decoder.decode(new Uint8Array(chunk));
	}
	return String(chunk);
}

function emitProcessStdio(channel: StdioChannel, chunk: unknown): boolean {
	if (activeProcessRequestId === null || activeExecutionId === null) {
		return true;
	}
	emitStdio(
		activeExecutionId,
		activeProcessRequestId,
		channel,
		normalizeProcessOutputChunk(chunk),
	);
	return true;
}

function createBrowserProcess(): Record<string, unknown> {
	type BrowserProcessListener = (value?: unknown) => void;
	type BrowserProcessListenerMap = Record<string, BrowserProcessListener[]>;
	type BrowserPtyStdio = {
		masterFd?: number;
		slaveFd: number;
		path?: string;
		columns: number;
		rows: number;
	};
	type BrowserStdin = {
		readable: boolean;
		paused: boolean;
		encoding: string | null;
		isRaw: boolean;
		read(size?: number): string | Uint8Array | null;
		on(event: string, listener: BrowserProcessListener): BrowserStdin;
		once(event: string, listener: BrowserProcessListener): BrowserStdin;
		off(event: string, listener: BrowserProcessListener): BrowserStdin;
		removeListener(
			event: string,
			listener: BrowserProcessListener,
		): BrowserStdin;
		emit(event: string, value?: unknown): boolean;
		pause(): BrowserStdin;
		resume(): BrowserStdin;
		setEncoding(encoding: string): BrowserStdin;
		setRawMode(mode: boolean): BrowserStdin;
		readonly readableLength: number;
		readonly isTTY: boolean;
		[Symbol.asyncIterator](): AsyncGenerator<string, void, void>;
	};
	type BrowserWritable = {
		readonly isTTY: boolean;
		readonly columns: number;
		readonly rows: number;
		write(chunk: unknown, encoding?: unknown, callback?: unknown): boolean;
		on(event: string, listener: BrowserProcessListener): BrowserWritable;
		once(event: string, listener: BrowserProcessListener): BrowserWritable;
		off(event: string, listener: BrowserProcessListener): BrowserWritable;
		removeListener(
			event: string,
			listener: BrowserProcessListener,
		): BrowserWritable;
		emit(event: string, value?: unknown): boolean;
	};

	let cwd = "/";
	let stdinData = "";
	let stdinPosition = 0;
	let stdinEnded = false;
	let stdinFlushQueued = false;
	let stdioPty: BrowserPtyStdio | null = null;
	let ptyPumpGeneration = 0;
	let ptyPumpScheduled = false;
	const stdinListeners: BrowserProcessListenerMap = Object.create(null);
	const stdinOnceListeners: BrowserProcessListenerMap = Object.create(null);

	const ttyState = {
		isatty(fd: unknown): boolean {
			return (
				stdioPty !== null &&
				(typeof fd === "number" || typeof fd === "string") &&
				(fd === 0 ||
					fd === 1 ||
					fd === 2 ||
					fd === "0" ||
					fd === "1" ||
					fd === "2")
			);
		},
		columns(): number {
			return stdioPty?.columns ?? 80;
		},
		rows(): number {
			return stdioPty?.rows ?? 24;
		},
	};
	(globalThis as Record<string, unknown>).__agentOSTtyState = ttyState;

	const parsePtyStdio = (value: unknown): BrowserPtyStdio | null => {
		if (!value || typeof value !== "object") return null;
		const record = value as Record<string, unknown>;
		let masterFd: number | undefined;
		let slaveFd = record.slaveFd;
		const columns = Number.isInteger(record.columns)
			? (record.columns as number)
			: 80;
		const rows = Number.isInteger(record.rows) ? (record.rows as number) : 24;
		let path: string | undefined;
		if (record.open === true) {
			const pair = ptySyncBridge().requestJson<{
				masterFd?: unknown;
				slaveFd?: unknown;
				path?: unknown;
			}>("pty.open", [{}]);
			if (!Number.isInteger(pair.masterFd) || !Number.isInteger(pair.slaveFd)) {
				throw new Error("pty.open returned invalid fd pair");
			}
			masterFd = pair.masterFd as number;
			slaveFd = pair.slaveFd;
			path = typeof pair.path === "string" ? pair.path : undefined;
			ptySyncBridge().requestVoid("pty.resize", [
				{
					fd: slaveFd as number,
					cols: Math.max(1, columns),
					rows: Math.max(1, rows),
				},
			]);
			if (activeExecutionId && activeProcessRequestId !== null) {
				postPtyOpened(activeExecutionId, activeProcessRequestId, {
					masterFd,
					slaveFd: slaveFd as number,
					path,
					columns: Math.max(1, columns),
					rows: Math.max(1, rows),
				});
			}
		}
		if (!Number.isInteger(slaveFd) || (slaveFd as number) < 0) return null;
		return {
			masterFd,
			slaveFd: slaveFd as number,
			path,
			columns: Math.max(1, columns),
			rows: Math.max(1, rows),
		};
	};

	const emitStdinListeners = (event: string, value?: unknown): boolean => {
		const listeners = [
			...(stdinListeners[event] ?? []),
			...(stdinOnceListeners[event] ?? []),
		];
		stdinOnceListeners[event] = [];
		for (const listener of listeners) {
			listener(value);
		}
		return listeners.length > 0;
	};

	const clearStdinListeners = (): void => {
		for (const key of Object.keys(stdinListeners)) {
			stdinListeners[key] = [];
		}
		for (const key of Object.keys(stdinOnceListeners)) {
			stdinOnceListeners[key] = [];
		}
	};

	const ptySyncBridge = () => {
		const syncBridge = activeSyncBridge;
		if (!syncBridge) {
			throw new Error("PTY stdio requires an active sync bridge");
		}
		return syncBridge;
	};

	const ptyBytesToProcessChunk = (bytes: Uint8Array): string | Uint8Array =>
		stdin.encoding ? decoder.decode(bytes) : bytes;

	const readPtyStdinOnce = (): boolean => {
		if (!stdioPty || stdinEnded || stdin.paused) return false;
		const result = ptySyncBridge().requestJson<{ data?: string | null }>(
			"pty.read",
			[{ fd: stdioPty.slaveFd, maxBytes: 4096, timeoutMs: 0 }],
		);
		if (typeof result.data !== "string") return false;
		emitStdinListeners(
			"data",
			ptyBytesToProcessChunk(base64ToBytes(result.data)),
		);
		return true;
	};

	const schedulePtyPump = (): void => {
		if (!stdioPty || stdinEnded || stdin.paused || ptyPumpScheduled) return;
		const generation = ptyPumpGeneration;
		ptyPumpScheduled = true;
		setTimeout(() => {
			ptyPumpScheduled = false;
			if (
				generation !== ptyPumpGeneration ||
				!stdioPty ||
				stdinEnded ||
				stdin.paused
			) {
				return;
			}
			try {
				readPtyStdinOnce();
			} finally {
				schedulePtyPump();
			}
		}, 0);
	};

	const writePtyStdio = (chunk: unknown, encoding?: unknown): boolean => {
		if (!stdioPty) return false;
		const bytes = toUint8Array(chunk);
		ptySyncBridge().requestJson("pty.write", [
			{ fd: stdioPty.slaveFd, data: bytes },
		]);
		return true;
	};

	const setPtyRawMode = (mode: boolean): void => {
		if (!stdioPty) return;
		ptySyncBridge().requestVoid("pty.tcsetattr", [
			mode
				? {
						fd: stdioPty.slaveFd,
						icrnl: false,
						opost: false,
						icanon: false,
						echo: false,
						isig: false,
					}
				: {
						fd: stdioPty.slaveFd,
						icrnl: true,
						opost: true,
						icanon: true,
						echo: true,
						isig: true,
					},
		]);
	};

	const stopPtyPump = (): void => {
		ptyPumpGeneration += 1;
		ptyPumpScheduled = false;
	};

	const flushStdin = (): void => {
		stdinFlushQueued = false;
		if (stdin.paused || stdinEnded) {
			return;
		}
		if (stdioPty) {
			readPtyStdinOnce();
			schedulePtyPump();
			return;
		}
		if (stdinPosition < stdinData.length) {
			const chunk = stdinData.slice(stdinPosition);
			stdinPosition = stdinData.length;
			emitStdinListeners("data", toProcessChunk(chunk, stdin.encoding));
		}
		// In streaming mode the host owns end-of-input (via end-stdin) — do not auto-end.
		if (!stdinEnded && !streamingStdinEnabled) {
			stdinEnded = true;
			emitStdinListeners("end");
			emitStdinListeners("close");
		}
	};
	// Host-driven streaming stdin (write-stdin / end-stdin messages) for this execution.
	activeStdinPush = (data: string): void => {
		if (stdinEnded) return;
		if (stdioPty) return;
		if (stdin.paused) stdin.paused = false;
		emitStdinListeners("data", toProcessChunk(data, stdin.encoding));
	};
	activeStdinEnd = (): void => {
		if (stdinEnded) return;
		if (stdioPty) return;
		stdinEnded = true;
		emitStdinListeners("end");
		emitStdinListeners("close");
	};

	const scheduleStdinFlush = (): void => {
		if (stdinFlushQueued) {
			return;
		}
		stdinFlushQueued = true;
		queueMicrotask(flushStdin);
	};

	const stdin: BrowserStdin = {
		readable: true,
		paused: true,
		encoding: null,
		isRaw: false,
		read(size?: number) {
			if (stdioPty) {
				const result = ptySyncBridge().requestJson<{ data?: string | null }>(
					"pty.read",
					[{ fd: stdioPty.slaveFd, maxBytes: size ?? 4096, timeoutMs: 0 }],
				);
				return typeof result.data === "string"
					? ptyBytesToProcessChunk(base64ToBytes(result.data))
					: null;
			}
			if (stdinPosition >= stdinData.length) {
				return null;
			}
			const chunk = size
				? stdinData.slice(stdinPosition, stdinPosition + size)
				: stdinData.slice(stdinPosition);
			stdinPosition += chunk.length;
			return toProcessChunk(chunk, stdin.encoding);
		},
		on(event, listener) {
			if (!stdinListeners[event]) {
				stdinListeners[event] = [];
			}
			stdinListeners[event].push(listener);
			if (event === "data" && stdin.paused) {
				stdin.resume();
			}
			return stdin;
		},
		once(event, listener) {
			if (!stdinOnceListeners[event]) {
				stdinOnceListeners[event] = [];
			}
			stdinOnceListeners[event].push(listener);
			if (event === "data" && stdin.paused) {
				stdin.resume();
			}
			return stdin;
		},
		off(event, listener) {
			if (!stdinListeners[event]) {
				return stdin;
			}
			stdinListeners[event] = stdinListeners[event].filter(
				(candidate) => candidate !== listener,
			);
			return stdin;
		},
		removeListener(event, listener) {
			return stdin.off(event, listener);
		},
		emit(event, value) {
			return emitStdinListeners(event, value);
		},
		pause() {
			stdin.paused = true;
			return stdin;
		},
		resume() {
			stdin.paused = false;
			if (stdioPty) schedulePtyPump();
			else scheduleStdinFlush();
			return stdin;
		},
		setEncoding(encoding) {
			stdin.encoding = encoding;
			return stdin;
		},
		setRawMode(mode) {
			setPtyRawMode(mode);
			stdin.isRaw = mode;
			return stdin;
		},
		get readableLength() {
			if (stdioPty) return 0;
			return encoder.encode(stdinData.slice(stdinPosition)).byteLength;
		},
		get isTTY() {
			return stdioPty !== null;
		},
		async *[Symbol.asyncIterator]() {
			const remaining = stdinData.slice(stdinPosition);
			for (const line of remaining.split("\n")) {
				if (line.length > 0) {
					yield line;
				}
			}
		},
	};

	const processListeners: BrowserProcessListenerMap = Object.create(null);
	const processOnceListeners: BrowserProcessListenerMap = Object.create(null);
	const stdoutListeners: BrowserProcessListenerMap = Object.create(null);
	const stdoutOnceListeners: BrowserProcessListenerMap = Object.create(null);
	const stderrListeners: BrowserProcessListenerMap = Object.create(null);
	const stderrOnceListeners: BrowserProcessListenerMap = Object.create(null);

	const requireProcessListener = (
		listener: BrowserProcessListener,
	): BrowserProcessListener => {
		if (typeof listener !== "function") {
			throw new TypeError("process listener must be a function");
		}
		return listener;
	};

	const processSignalListenerCount = (event: string): number => {
		return (
			(processListeners[event]?.length ?? 0) +
			(processOnceListeners[event]?.length ?? 0)
		);
	};

	const syncProcessSignalState = (
		event: string,
		action: "default" | "user",
	): void => {
		const signal = signalNumberForEvent(event);
		if (signal === null) {
			return;
		}
		const syncBridge = activeSyncBridge;
		if (!syncBridge) {
			return;
		}
		syncBridge.requestVoid("process.signal_state", [signal, action, "[]", 0]);
	};

	const maybeSyncProcessSignalTransition = (
		event: string,
		before: number,
		after: number,
	): void => {
		if (before === 0 && after > 0) {
			syncProcessSignalState(event, "user");
		} else if (before > 0 && after === 0) {
			syncProcessSignalState(event, "default");
		}
	};

	const processOn = (
		event: string,
		listener: BrowserProcessListener,
		once: boolean,
	): Record<string, unknown> => {
		requireProcessListener(listener);
		const before = processSignalListenerCount(event);
		const map = once ? processOnceListeners : processListeners;
		if (!map[event]) {
			map[event] = [];
		}
		map[event].push(listener);
		maybeSyncProcessSignalTransition(
			event,
			before,
			processSignalListenerCount(event),
		);
		return processBridge;
	};

	const processOff = (
		event: string,
		listener: BrowserProcessListener,
	): Record<string, unknown> => {
		requireProcessListener(listener);
		const before = processSignalListenerCount(event);
		if (processListeners[event]) {
			processListeners[event] = processListeners[event].filter(
				(candidate) => candidate !== listener,
			);
		}
		if (processOnceListeners[event]) {
			processOnceListeners[event] = processOnceListeners[event].filter(
				(candidate) => candidate !== listener,
			);
		}
		maybeSyncProcessSignalTransition(
			event,
			before,
			processSignalListenerCount(event),
		);
		return processBridge;
	};

	const emitProcessListeners = (event: string, value?: unknown): boolean => {
		const before = processSignalListenerCount(event);
		const listeners = [
			...(processListeners[event] ?? []),
			...(processOnceListeners[event] ?? []),
		];
		processOnceListeners[event] = [];
		for (const listener of listeners) {
			listener(value);
		}
		maybeSyncProcessSignalTransition(
			event,
			before,
			processSignalListenerCount(event),
		);
		return listeners.length > 0;
	};

	const requireStreamListener = (
		listener: BrowserProcessListener,
	): BrowserProcessListener => {
		if (typeof listener !== "function") {
			throw new TypeError("stream listener must be a function");
		}
		return listener;
	};

	const streamOn = (
		listeners: BrowserProcessListenerMap,
		event: string,
		listener: BrowserProcessListener,
		stream: BrowserWritable,
	): BrowserWritable => {
		requireStreamListener(listener);
		if (!listeners[event]) {
			listeners[event] = [];
		}
		listeners[event].push(listener);
		return stream;
	};

	const streamOff = (
		listeners: BrowserProcessListenerMap,
		event: string,
		listener: BrowserProcessListener,
		stream: BrowserWritable,
	): BrowserWritable => {
		requireStreamListener(listener);
		if (listeners[event]) {
			listeners[event] = listeners[event].filter(
				(candidate) => candidate !== listener,
			);
		}
		return stream;
	};

	const emitStreamListeners = (
		listeners: BrowserProcessListenerMap,
		onceListeners: BrowserProcessListenerMap,
		event: string,
		value?: unknown,
	): boolean => {
		const callbacks = [
			...(listeners[event] ?? []),
			...(onceListeners[event] ?? []),
		];
		onceListeners[event] = [];
		for (const listener of callbacks) {
			listener(value);
		}
		return callbacks.length > 0;
	};

	const clearStreamListeners = (
		listeners: BrowserProcessListenerMap,
		onceListeners: BrowserProcessListenerMap,
	): void => {
		for (const key of Object.keys(listeners)) {
			listeners[key] = [];
		}
		for (const key of Object.keys(onceListeners)) {
			onceListeners[key] = [];
		}
	};

	const makeWritable = (
		channel: "stdout" | "stderr",
		listeners: BrowserProcessListenerMap,
		onceListeners: BrowserProcessListenerMap,
	): BrowserWritable => {
		const writable: BrowserWritable = {
			get isTTY() {
				return stdioPty !== null;
			},
			get columns() {
				return stdioPty?.columns ?? 80;
			},
			get rows() {
				return stdioPty?.rows ?? 24;
			},
			// Node signature: write(chunk[, encoding][, callback]). The callback MUST be
			// invoked on completion — code that awaits it (e.g. a WHATWG WritableStream
			// wrapping process.stdout) otherwise blocks after the first write.
			write(chunk: unknown, encoding?: unknown, callback?: unknown) {
				const result = stdioPty
					? writePtyStdio(chunk, encoding)
					: emitProcessStdio(channel, chunk);
				const cb = typeof encoding === "function" ? encoding : callback;
				if (typeof cb === "function") (cb as (err?: unknown) => void)();
				return result;
			},
			on(event, listener) {
				return streamOn(listeners, String(event), listener, writable);
			},
			once(event, listener) {
				return streamOn(onceListeners, String(event), listener, writable);
			},
			off(event, listener) {
				return streamOff(listeners, String(event), listener, writable);
			},
			removeListener(event, listener) {
				streamOff(listeners, String(event), listener, writable);
				streamOff(onceListeners, String(event), listener, writable);
				return writable;
			},
			emit(event, value) {
				return emitStreamListeners(
					listeners,
					onceListeners,
					String(event),
					value,
				);
			},
		};
		return writable;
	};

	const stdout = makeWritable("stdout", stdoutListeners, stdoutOnceListeners);
	const stderr = makeWritable("stderr", stderrListeners, stderrOnceListeners);

	const processBridge = {
		browser: true,
		env: {} as Record<string, string>,
		argv: ["node"],
		argv0: "node",
		pid: 1,
		ppid: 0,
		uid: 1000,
		gid: 1000,
		platform: "browser",
		arch: "x64",
		version: "v22.0.0",
		versions: {
			node: "22.0.0",
			// Guest crypto is served by pure-Rust crates, not OpenSSL. We still
			// surface the OpenSSL release vendored by the sidecar so guests that
			// read process.versions.openssl keep working, and the native V8 runtime
			// reports the same constant for parity.
			openssl: "3.6.2",
		},
		stdin,
		stdout,
		stderr,
		exitCode: 0,
		cwd: () => cwd,
		chdir: (nextCwd: string) => {
			cwd = String(nextCwd);
		},
		getuid: () => processBridge.uid,
		getgid: () => processBridge.gid,
		geteuid: () => processBridge.uid,
		getegid: () => processBridge.gid,
		getgroups: () => [processBridge.gid],
		nextTick: (callback: (...args: unknown[]) => void, ...args: unknown[]) => {
			queueMicrotask(() => callback(...args));
		},
		exit(code?: number) {
			const exitCode =
				typeof code === "number" ? code : (processBridge.exitCode ?? 0);
			processBridge.exitCode = exitCode;
			// Persistent execution: resolve the run (the call may come from an async
			// callback whose throw the sync exec wrapper cannot catch). Otherwise throw
			// the sentinel the run-to-completion exec wrapper unwinds on.
			if (persistentExitResolver) {
				const resolve = persistentExitResolver;
				persistentExitResolver = null;
				resolve(exitCode);
				return;
			}
			throw new Error(`process.exit(${exitCode})`);
		},
		kill(pid: number, signal: string | number = "SIGTERM") {
			if (pid !== processBridge.pid) {
				throw new Error(
					`process.kill only supports the current browser process pid (${processBridge.pid})`,
				);
			}
			const event =
				typeof signal === "number"
					? eventForSignalNumber(signal)
					: String(signal);
			if (event === "SIGWINCH") {
				stdout.emit("resize");
				stderr.emit("resize");
			}
			return emitProcessListeners(event);
		},
		on(event: string, listener: BrowserProcessListener) {
			return processOn(String(event), listener, false);
		},
		once(event: string, listener: BrowserProcessListener) {
			return processOn(String(event), listener, true);
		},
		off(event: string, listener: BrowserProcessListener) {
			return processOff(String(event), listener);
		},
		removeListener(event: string, listener: BrowserProcessListener) {
			return processOff(String(event), listener);
		},
		emit(event: string, value?: unknown) {
			return emitProcessListeners(String(event), value);
		},
		__agentOsRefreshProcess(nextConfig?: Record<string, unknown>) {
			stopPtyPump();
			clearStdinListeners();
			for (const key of Object.keys(processListeners)) {
				processListeners[key] = [];
			}
			for (const key of Object.keys(processOnceListeners)) {
				processOnceListeners[key] = [];
			}
			clearStreamListeners(stdoutListeners, stdoutOnceListeners);
			clearStreamListeners(stderrListeners, stderrOnceListeners);
			stdinData = typeof nextConfig?.stdin === "string" ? nextConfig.stdin : "";
			stdinPosition = 0;
			stdinEnded = false;
			stdinFlushQueued = false;
			stdin.paused = true;
			stdin.encoding = null;
			stdin.isRaw = false;
			stdioPty = parsePtyStdio(nextConfig?.stdioPty);
			processBridge.exitCode = 0;
			processBridge.env =
				nextConfig?.env && typeof nextConfig.env === "object"
					? { ...(nextConfig.env as Record<string, string>) }
					: {};
			if (typeof nextConfig?.cwd === "string") {
				cwd = nextConfig.cwd;
			}
			processBridge.argv = Array.isArray(nextConfig?.argv)
				? nextConfig.argv.map((value) => String(value))
				: ["node"];
			processBridge.argv0 = processBridge.argv[0] ?? "node";
			if (typeof nextConfig?.platform === "string") {
				processBridge.platform = nextConfig.platform;
			}
			if (typeof nextConfig?.arch === "string") {
				processBridge.arch = nextConfig.arch;
			}
			if (typeof nextConfig?.version === "string") {
				processBridge.version = nextConfig.version;
				processBridge.versions.node = nextConfig.version.replace(/^v/, "");
			}
			if (typeof nextConfig?.pid === "number") {
				processBridge.pid = nextConfig.pid;
			}
			if (typeof nextConfig?.ppid === "number") {
				processBridge.ppid = nextConfig.ppid;
			}
			if (typeof nextConfig?.uid === "number") {
				processBridge.uid = nextConfig.uid;
			}
			if (typeof nextConfig?.gid === "number") {
				processBridge.gid = nextConfig.gid;
			}
		},
		__agentOsStopPtyStdio() {
			stopPtyPump();
			stdioPty = null;
		},
		__agentOsResizePty(columns: number, rows: number) {
			if (!stdioPty) return;
			stdioPty.columns = Math.max(1, Math.trunc(columns));
			stdioPty.rows = Math.max(1, Math.trunc(rows));
			stdout.emit("resize");
			stderr.emit("resize");
			emitProcessListeners("SIGWINCH");
		},
	};

	return processBridge;
}

function getRuntimeProcess(): Record<string, unknown> | undefined {
	const proc = (globalThis as Record<string, unknown>).process;
	if (!proc || typeof proc !== "object") {
		return undefined;
	}
	return proc as Record<string, unknown>;
}

function refreshRuntimeProcess(): void {
	const proc = getRuntimeProcess();
	const refresh = proc?.__agentOsRefreshProcess as
		| ((nextConfig?: Record<string, unknown> | null) => void)
		| undefined;
	if (typeof refresh === "function") {
		refresh(runtimeProcessConfig);
	}
}

function resizeRuntimePty(
	executionId: string,
	columns: number,
	rows: number,
): void {
	if (executionId !== activeExecutionId) {
		return;
	}
	const proc = getRuntimeProcess();
	const resize = proc?.__agentOsResizePty as
		| ((columns: number, rows: number) => void)
		| undefined;
	if (typeof resize === "function") {
		resize(columns, rows);
	}
}

function ensureProcessGlobal(): void {
	if (getRuntimeProcess()) {
		refreshRuntimeProcess();
		return;
	}

	exposeMutableRuntimeStateGlobal("process", createBrowserProcess());
	refreshRuntimeProcess();
}

function updateProcessConfig(
	options: BrowserWorkerExecOptions | undefined,
	timingMitigation: TimingMitigation,
	frozenTimeMs?: number,
): void {
	if (runtimeProcessConfig) {
		runtimeProcessConfig.timingMitigation = timingMitigation;
		if (frozenTimeMs === undefined) {
			delete runtimeProcessConfig.frozenTimeMs;
		} else {
			runtimeProcessConfig.frozenTimeMs = frozenTimeMs;
		}
		runtimeProcessConfig.stdin = options?.stdin ?? "";
		if (options?.stdioPty) {
			runtimeProcessConfig.stdioPty = options.stdioPty;
		} else {
			delete runtimeProcessConfig.stdioPty;
		}
		if (options?.env) {
			// Per-exec env is already filtered by the trusted driver.
			const currentEnv =
				runtimeProcessConfig.env && typeof runtimeProcessConfig.env === "object"
					? (runtimeProcessConfig.env as Record<string, string>)
					: {};
			runtimeProcessConfig.env = { ...currentEnv, ...options.env };
		}
	}

	refreshRuntimeProcess();

	const proc = getRuntimeProcess();
	if (!proc) return;
	proc.exitCode = 0;
	proc.timingMitigation = timingMitigation;
	if (frozenTimeMs === undefined) {
		delete proc.frozenTimeMs;
	} else {
		proc.frozenTimeMs = frozenTimeMs;
	}
	if (options?.cwd && typeof proc.chdir === "function") {
		exposeMutableRuntimeStateGlobal("__runtimeProcessCwdOverride", options.cwd);
		globalEval(getIsolateRuntimeSource("overrideProcessCwd"));
		try {
			proc.chdir(options.cwd);
		} catch (error) {
			if (
				!(
					error instanceof Error &&
					error.message.includes("process.chdir() is not supported in workers")
				)
			) {
				throw error;
			}
		}
	}
}

/**
 * Execute user code as a script (process-style). Transforms ESM/dynamic
 * imports, sets up module/exports globals, and waits for active handles.
 */
async function execScript(
	executionId: string,
	requestId: number,
	code: string,
	options?: BrowserWorkerExecOptions,
	captureStdio = false,
): Promise<ExecResult> {
	resetModuleState(options?.cwd ?? "/");
	const timingMitigation = options?.timingMitigation ?? runtimeTimingMitigation;
	const frozenTimeMs = applyTimingMitigation(timingMitigation);
	const previousProcessRequestId = activeProcessRequestId;
	const previousExecutionId = activeExecutionId;
	const previousCaptureStdio = activeCaptureStdio;
	activeProcessRequestId = requestId;
	activeExecutionId = executionId;
	activeCaptureStdio = captureStdio;
	persistentExitResolver = null;
	streamingStdinEnabled = Boolean(options?.streamingStdin);
	updateProcessConfig(options, timingMitigation, frozenTimeMs);
	setDynamicImportFallback();
	try {
		const scriptResult = (async (): Promise<ExecResult> => {
			let transformed = code;
			if (isESM(code, options?.filePath)) {
				transformed = transform(transformed, { transforms: ["imports"] }).code;
			}
			transformed = transformDynamicImport(transformed);

			exposeMutableRuntimeStateGlobal("module", { exports: {} });
			const moduleRef = (globalThis as Record<string, unknown>).module as {
				exports?: unknown;
			};
			exposeMutableRuntimeStateGlobal("exports", moduleRef.exports);

			if (options?.filePath) {
				const dirname = options.filePath.includes("/")
					? options.filePath.substring(0, options.filePath.lastIndexOf("/")) ||
						"/"
					: "/";
				exposeMutableRuntimeStateGlobal("__filename", options.filePath);
				exposeMutableRuntimeStateGlobal("__dirname", dirname);
				exposeMutableRuntimeStateGlobal("_currentModule", {
					dirname,
					filename: options.filePath,
				});
			}

			// Persistent (service) program: arm the exit resolver BEFORE eval so an exit
			// that fires while we await (e.g. on stdin EOF, from an async stream callback)
			// resolves the run rather than throwing uncaught. The worker event loop stays
			// alive for async I/O (stdin events, timers, stream pumps) while we await.
			const persistentExitPromise = options?.persistent
				? new Promise<number>((resolve) => {
						persistentExitResolver = resolve;
					})
				: null;

			// Await the eval result so async IIFEs / top-level promise expressions
			// resolve before we check for active handles.
			const evalResult = globalEval(transformed);
			if (
				evalResult &&
				typeof evalResult === "object" &&
				typeof (evalResult as Record<string, unknown>).then === "function"
			) {
				await evalResult;
			}
			await Promise.resolve();

			const currentExitCode = () =>
				(
					(globalThis as Record<string, unknown>).process as {
						exitCode?: number;
					}
				)?.exitCode ?? 0;

			if (persistentExitPromise) {
				const code = await Promise.race([
					persistentExitPromise,
					new Promise<number>((resolve) =>
						setTimeout(() => {
							persistentExitResolver = null;
							resolve(currentExitCode());
						}, PERSISTENT_EXEC_TIMEOUT_MS),
					),
				]);
				// Drain remaining microtasks + a macrotask turn so any final async output
				// (the program's last stdout writes) flushes before teardown.
				await new Promise((resolve) => setTimeout(resolve, 0));
				await new Promise((resolve) => setTimeout(resolve, 0));
				return { code };
			}

			const waitForActiveHandles = (globalThis as Record<string, unknown>)
				._waitForActiveHandles as (() => Promise<void>) | undefined;
			if (typeof waitForActiveHandles === "function") {
				await waitForActiveHandles();
			}

			return {
				code: currentExitCode(),
			};
		})();
		const signalResult = new Promise<ExecResult>((resolve) => {
			pendingExecutionSignals.set(executionId, (signal) => {
				const code = defaultSignalExitCode(signal);
				resolve({ code: code ?? 0 });
			});
		});

		return await Promise.race([scriptResult, signalResult]);
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err);
		const exitMatch = message.match(/process\.exit\((\d+)\)/);
		if (exitMatch) {
			const exitCode = Number.parseInt(exitMatch[1], 10);
			return {
				code: exitCode,
			};
		}
		// Include the stack (when present) so a guest program's load/runtime failure is
		// diagnosable — a bare message ("argument must be of type Function") is useless
		// for locating which call in a large bundle threw.
		const detail = err instanceof Error && err.stack ? err.stack : message;
		return {
			code: 1,
			errorMessage: boundErrorMessage(detail),
		};
	} finally {
		const proc = getRuntimeProcess() as
			| { __agentOsStopPtyStdio?: () => void }
			| undefined;
		proc?.__agentOsStopPtyStdio?.();
		pendingExecutionSignals.delete(executionId);
		activeProcessRequestId = previousProcessRequestId;
		activeExecutionId = previousExecutionId;
		activeCaptureStdio = previousCaptureStdio;
		streamingStdinEnabled = false;
		activeStdinPush = null;
		activeStdinEnd = null;
	}
}

async function runScript<T = unknown>(
	executionId: string,
	requestId: number,
	code: string,
	filePath?: string,
	captureStdio = false,
): Promise<RunResult<T>> {
	const execResult = await execScript(
		executionId,
		requestId,
		code,
		{ filePath },
		captureStdio,
	);
	const moduleObj = (globalThis as Record<string, unknown>).module as {
		exports?: T;
	};
	return {
		...execResult,
		exports: moduleObj?.exports,
	};
}

self.onmessage = async (event: MessageEvent<BrowserWorkerRequestMessage>) => {
	const message = event.data;
	try {
		if (message.type === "init") {
			if (
				typeof message.controlToken !== "string" ||
				message.controlToken.length === 0
			) {
				return;
			}
			if (controlToken && message.controlToken !== controlToken) {
				return;
			}
			controlToken = message.controlToken;
			await initRuntime(message.payload);
			postResponse({
				type: "response",
				id: message.id,
				ok: true,
				result: true,
			});
			return;
		}
		if (!controlToken || message.controlToken !== controlToken) {
			return;
		}
		if (!initialized) {
			throw new Error("Sandbox worker not initialized");
		}
		if (message.type === "exec") {
			postAsyncResponse(
				message.id,
				execScript(
					message.payload.executionId,
					message.id,
					message.payload.code,
					message.payload.options,
					message.payload.captureStdio,
				),
			);
			return;
		}
		// Host-driven streaming stdin for the active persistent execution.
		if (message.type === "write-stdin") {
			activeStdinPush?.(message.data);
			return;
		}
		if (message.type === "end-stdin") {
			activeStdinEnd?.();
			return;
		}
		if (message.type === "resize-pty") {
			resizeRuntimePty(message.executionId, message.columns, message.rows);
			return;
		}
		if (message.type === "run") {
			postAsyncResponse(
				message.id,
				runScript(
					message.payload.executionId,
					message.id,
					message.payload.code,
					message.payload.filePath,
					message.payload.captureStdio,
				),
			);
			return;
		}
		if (message.type === "signal") {
			const signal = Number(message.payload.signal);
			const resolveSignal = pendingExecutionSignals.get(
				message.payload.executionId,
			);
			if (resolveSignal && Number.isInteger(signal)) {
				resolveSignal(signal);
			}
			return;
		}
		if (message.type === "extension") {
			const error = new Error(
				`Browser worker extension dispatch is not implemented for namespace ${message.payload.namespace}`,
			) as Error & { code?: string };
			error.code = "ERR_AGENTOS_BROWSER_EXTENSION_UNSUPPORTED";
			throw error;
		}
		if (message.type === "dispose") {
			postResponse({
				type: "response",
				id: message.id,
				ok: true,
				result: true,
			});
			close();
		}
	} catch (err) {
		const error = err as { message?: string; stack?: string; code?: string };
		postResponse({
			type: "response",
			id: message.id,
			ok: false,
			error: {
				message: error?.message ?? String(err),
				stack: error?.stack,
				code: error?.code,
			},
		});
	}
};
