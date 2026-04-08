// Early polyfills - this file must be imported FIRST before any other modules
// that might use TextEncoder/TextDecoder/EventTarget at module scope.

type SupportedEncoding = "utf-8" | "utf-16le" | "utf-16be";

type DecodedChunk = {
	text: string;
	pending: number[];
};

type EventListenerLike =
	| ((event: PatchedEvent) => void)
	| { handleEvent?: (event: PatchedEvent) => void };

type ListenerRecord = {
	listener: EventListenerLike;
	capture: boolean;
	once: boolean;
	passive: boolean;
	kind: "function" | "object";
	signal?: AbortSignal;
	abortListener?: () => void;
};

function defineGlobal(name: string, value: unknown): void {
	(globalThis as Record<string, unknown>)[name] = value;
}

if (typeof globalThis.global === "undefined") {
	defineGlobal("global", globalThis);
}

if (
	typeof globalThis.RegExp === "function" &&
	!("__secureExecRgiEmojiCompat" in globalThis.RegExp)
) {
	const NativeRegExp = globalThis.RegExp;
	const rgiEmojiPattern = "^\\p{RGI_Emoji}$";
	const rgiEmojiBaseClass =
		"[\\u{00A9}\\u{00AE}\\u{203C}\\u{2049}\\u{2122}\\u{2139}\\u{2194}-\\u{21AA}\\u{231A}-\\u{23FF}\\u{24C2}\\u{25AA}-\\u{27BF}\\u{2934}-\\u{2935}\\u{2B05}-\\u{2B55}\\u{3030}\\u{303D}\\u{3297}\\u{3299}\\u{1F000}-\\u{1FAFF}]";
	const rgiEmojiKeycap = "[#*0-9]\\uFE0F?\\u20E3";
	const rgiEmojiFallbackSource =
		"^(?:" +
		rgiEmojiKeycap +
		"|\\p{Regional_Indicator}{2}|" +
		rgiEmojiBaseClass +
		"(?:\\uFE0F|\\u200D(?:" +
		rgiEmojiKeycap +
		"|" +
		rgiEmojiBaseClass +
		")|[\\u{1F3FB}-\\u{1F3FF}])*)$";

	try {
		new NativeRegExp(rgiEmojiPattern, "v");
	} catch (error) {
		if (String((error as Error)?.message ?? error).includes("RGI_Emoji")) {
			const CompatRegExp = function CompatRegExp(
				pattern?: string | RegExp,
				flags?: string,
			): RegExp {
				const normalizedPattern =
					pattern instanceof NativeRegExp && flags === undefined
						? pattern.source
						: String(pattern);
				const normalizedFlags =
					flags === undefined
						? (pattern instanceof NativeRegExp ? pattern.flags : "")
						: String(flags);

				try {
					return new NativeRegExp(pattern as string | RegExp, flags);
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
				configurable: true,
			});
			defineGlobal(
				"RegExp",
				Object.assign(CompatRegExp, { __secureExecRgiEmojiCompat: true }),
			);
		}
	}
}

function withCode<T extends Error>(error: T, code: string): T & { code: string } {
	(error as T & { code: string }).code = code;
	return error as T & { code: string };
}

function createEncodingNotSupportedError(label: string): RangeError & { code: string } {
	return withCode(
		new RangeError(`The "${label}" encoding is not supported`),
		"ERR_ENCODING_NOT_SUPPORTED",
	);
}

function createEncodingInvalidDataError(
	encoding: SupportedEncoding,
): TypeError & { code: string } {
	return withCode(
		new TypeError(`The encoded data was not valid for encoding ${encoding}`),
		"ERR_ENCODING_INVALID_ENCODED_DATA",
	);
}

function createInvalidDecodeInputError(): TypeError & { code: string } {
	return withCode(
		new TypeError(
			'The "input" argument must be an instance of ArrayBuffer, SharedArrayBuffer, or ArrayBufferView.',
		),
		"ERR_INVALID_ARG_TYPE",
	);
}

function trimAsciiWhitespace(value: string): string {
	return value.replace(/^[\t\n\f\r ]+|[\t\n\f\r ]+$/g, "");
}

function normalizeEncodingLabel(label?: unknown): SupportedEncoding {
	const normalized = trimAsciiWhitespace(
		label === undefined ? "utf-8" : String(label),
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

function toUint8Array(input?: unknown): Uint8Array {
	if (input === undefined) {
		return new Uint8Array(0);
	}

	if (ArrayBuffer.isView(input)) {
		return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
	}

	if (input instanceof ArrayBuffer) {
		return new Uint8Array(input);
	}

	if (
		typeof SharedArrayBuffer !== "undefined" &&
		input instanceof SharedArrayBuffer
	) {
		return new Uint8Array(input);
	}

	throw createInvalidDecodeInputError();
}

function encodeUtf8ScalarValue(codePoint: number, bytes: number[]): void {
	if (codePoint <= 0x7f) {
		bytes.push(codePoint);
		return;
	}
	if (codePoint <= 0x7ff) {
		bytes.push(0xc0 | (codePoint >> 6), 0x80 | (codePoint & 0x3f));
		return;
	}
	if (codePoint <= 0xffff) {
		bytes.push(
			0xe0 | (codePoint >> 12),
			0x80 | ((codePoint >> 6) & 0x3f),
			0x80 | (codePoint & 0x3f),
		);
		return;
	}
	bytes.push(
		0xf0 | (codePoint >> 18),
		0x80 | ((codePoint >> 12) & 0x3f),
		0x80 | ((codePoint >> 6) & 0x3f),
		0x80 | (codePoint & 0x3f),
	);
}

function encodeUtf8(input = ""): Uint8Array {
	const value = String(input);
	const bytes: number[] = [];
	for (let index = 0; index < value.length; index += 1) {
		const codeUnit = value.charCodeAt(index);
		if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
			const nextIndex = index + 1;
			if (nextIndex < value.length) {
				const nextCodeUnit = value.charCodeAt(nextIndex);
				if (nextCodeUnit >= 0xdc00 && nextCodeUnit <= 0xdfff) {
					const codePoint =
						0x10000 + ((codeUnit - 0xd800) << 10) + (nextCodeUnit - 0xdc00);
					encodeUtf8ScalarValue(codePoint, bytes);
					index = nextIndex;
					continue;
				}
			}
			encodeUtf8ScalarValue(0xfffd, bytes);
			continue;
		}
		if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) {
			encodeUtf8ScalarValue(0xfffd, bytes);
			continue;
		}
		encodeUtf8ScalarValue(codeUnit, bytes);
	}
	return new Uint8Array(bytes);
}

function appendCodePoint(output: string[], codePoint: number): void {
	if (codePoint <= 0xffff) {
		output.push(String.fromCharCode(codePoint));
		return;
	}
	const adjusted = codePoint - 0x10000;
	output.push(
		String.fromCharCode(0xd800 + (adjusted >> 10)),
		String.fromCharCode(0xdc00 + (adjusted & 0x3ff)),
	);
}

function isContinuationByte(value: number): boolean {
	return value >= 0x80 && value <= 0xbf;
}

function decodeUtf8(
	bytes: Uint8Array,
	fatal: boolean,
	stream: boolean,
	encoding: SupportedEncoding,
): DecodedChunk {
	const output: string[] = [];

	for (let index = 0; index < bytes.length; ) {
		const first = bytes[index];

		if (first <= 0x7f) {
			output.push(String.fromCharCode(first));
			index += 1;
			continue;
		}

		let needed = 0;
		let codePoint = 0;

		if (first >= 0xc2 && first <= 0xdf) {
			needed = 1;
			codePoint = first & 0x1f;
		} else if (first >= 0xe0 && first <= 0xef) {
			needed = 2;
			codePoint = first & 0x0f;
		} else if (first >= 0xf0 && first <= 0xf4) {
			needed = 3;
			codePoint = first & 0x07;
		} else {
			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			index += 1;
			continue;
		}

		if (index + needed >= bytes.length) {
			if (stream) {
				return {
					text: output.join(""),
					pending: Array.from(bytes.slice(index)),
				};
			}
			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			break;
		}

		const second = bytes[index + 1];
		if (!isContinuationByte(second)) {
			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			index += 1;
			continue;
		}

		if (
			(first === 0xe0 && second < 0xa0) ||
			(first === 0xed && second > 0x9f) ||
			(first === 0xf0 && second < 0x90) ||
			(first === 0xf4 && second > 0x8f)
		) {
			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			index += 1;
			continue;
		}

		codePoint = (codePoint << 6) | (second & 0x3f);

		if (needed >= 2) {
			const third = bytes[index + 2];
			if (!isContinuationByte(third)) {
				if (fatal) {
					throw createEncodingInvalidDataError(encoding);
				}
				output.push("\ufffd");
				index += 1;
				continue;
			}
			codePoint = (codePoint << 6) | (third & 0x3f);
		}

		if (needed === 3) {
			const fourth = bytes[index + 3];
			if (!isContinuationByte(fourth)) {
				if (fatal) {
					throw createEncodingInvalidDataError(encoding);
				}
				output.push("\ufffd");
				index += 1;
				continue;
			}
			codePoint = (codePoint << 6) | (fourth & 0x3f);
		}

		if (codePoint >= 0xd800 && codePoint <= 0xdfff) {
			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			index += needed + 1;
			continue;
		}

		appendCodePoint(output, codePoint);
		index += needed + 1;
	}

	return { text: output.join(""), pending: [] };
}

function decodeUtf16(
	bytes: Uint8Array,
	encoding: SupportedEncoding,
	fatal: boolean,
	stream: boolean,
	bomSeen: boolean,
): DecodedChunk {
	const output: string[] = [];
	let endian: "le" | "be" = encoding === "utf-16be" ? "be" : "le";

	if (!bomSeen && encoding === "utf-16le" && bytes.length >= 2) {
		if (bytes[0] === 0xfe && bytes[1] === 0xff) {
			endian = "be";
		}
	}

	for (let index = 0; index < bytes.length; ) {
		if (index + 1 >= bytes.length) {
			if (stream) {
				return {
					text: output.join(""),
					pending: Array.from(bytes.slice(index)),
				};
			}
			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			break;
		}

		const first = bytes[index];
		const second = bytes[index + 1];
		const codeUnit =
			endian === "le" ? first | (second << 8) : (first << 8) | second;
		index += 2;

		if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
			if (index + 1 >= bytes.length) {
				if (stream) {
					return {
						text: output.join(""),
						pending: Array.from(bytes.slice(index - 2)),
					};
				}
				if (fatal) {
					throw createEncodingInvalidDataError(encoding);
				}
				output.push("\ufffd");
				continue;
			}

			const nextFirst = bytes[index];
			const nextSecond = bytes[index + 1];
			const nextCodeUnit =
				endian === "le"
					? nextFirst | (nextSecond << 8)
					: (nextFirst << 8) | nextSecond;

			if (nextCodeUnit >= 0xdc00 && nextCodeUnit <= 0xdfff) {
				const codePoint =
					0x10000 +
					((codeUnit - 0xd800) << 10) +
					(nextCodeUnit - 0xdc00);
				appendCodePoint(output, codePoint);
				index += 2;
				continue;
			}

			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			continue;
		}

		if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) {
			if (fatal) {
				throw createEncodingInvalidDataError(encoding);
			}
			output.push("\ufffd");
			continue;
		}

		output.push(String.fromCharCode(codeUnit));
	}

	return { text: output.join(""), pending: [] };
}

class PatchedTextEncoder {
	encode(input = ""): Uint8Array {
		return encodeUtf8(input);
	}

	encodeInto(input: string, destination: Uint8Array): { read: number; written: number } {
		const value = String(input);
		let read = 0;
		let written = 0;

		for (let index = 0; index < value.length; index += 1) {
			const codeUnit = value.charCodeAt(index);
			let chunk = "";

			if (
				codeUnit >= 0xd800 &&
				codeUnit <= 0xdbff &&
				index + 1 < value.length
			) {
				const nextCodeUnit = value.charCodeAt(index + 1);
				if (nextCodeUnit >= 0xdc00 && nextCodeUnit <= 0xdfff) {
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

	get encoding(): string {
		return "utf-8";
	}

	get [Symbol.toStringTag](): string {
		return "TextEncoder";
	}
}

class PatchedTextDecoder {
	private readonly normalizedEncoding: SupportedEncoding;
	private readonly fatalFlag: boolean;
	private readonly ignoreBOMFlag: boolean;
	private pendingBytes: number[] = [];
	private bomSeen = false;

	constructor(label?: unknown, options?: { fatal?: boolean; ignoreBOM?: boolean } | null) {
		const normalizedOptions = options == null ? {} : Object(options);
		this.normalizedEncoding = normalizeEncodingLabel(label);
		this.fatalFlag = Boolean(
			(normalizedOptions as { fatal?: boolean }).fatal,
		);
		this.ignoreBOMFlag = Boolean(
			(normalizedOptions as { ignoreBOM?: boolean }).ignoreBOM,
		);
	}

	get encoding(): string {
		return this.normalizedEncoding;
	}

	get fatal(): boolean {
		return this.fatalFlag;
	}

	get ignoreBOM(): boolean {
		return this.ignoreBOMFlag;
	}

	get [Symbol.toStringTag](): string {
		return "TextDecoder";
	}

	decode(
		input?: unknown,
		options?: { stream?: boolean } | null,
	): string {
		const normalizedOptions = options == null ? {} : Object(options);
		const stream = Boolean(
			(normalizedOptions as { stream?: boolean }).stream,
		);
		const incoming = toUint8Array(input);
		const merged = new Uint8Array(this.pendingBytes.length + incoming.length);
		merged.set(this.pendingBytes, 0);
		merged.set(incoming, this.pendingBytes.length);

		const decoded =
			this.normalizedEncoding === "utf-8"
				? decodeUtf8(
						merged,
						this.fatalFlag,
						stream,
						this.normalizedEncoding,
				  )
				: decodeUtf16(
						merged,
						this.normalizedEncoding,
						this.fatalFlag,
						stream,
						this.bomSeen,
				  );

		this.pendingBytes = decoded.pending;

		let text = decoded.text;
		if (!this.bomSeen && text.length > 0) {
			if (!this.ignoreBOMFlag && text.charCodeAt(0) === 0xfeff) {
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
			return text + "\ufffd".repeat(Math.ceil(pending.length / 2));
		}

		return text;
	}
}

function normalizeAddEventListenerOptions(options: unknown): {
	capture: boolean;
	once: boolean;
	passive: boolean;
	signal?: AbortSignal;
} {
	if (typeof options === "boolean") {
		return {
			capture: options,
			once: false,
			passive: false,
		};
	}

	if (options == null) {
		return {
			capture: false,
			once: false,
			passive: false,
		};
	}

	const normalized = Object(options) as {
		capture?: boolean;
		once?: boolean;
		passive?: boolean;
		signal?: AbortSignal;
	};

	return {
		capture: Boolean(normalized.capture),
		once: Boolean(normalized.once),
		passive: Boolean(normalized.passive),
		signal: normalized.signal,
	};
}

function normalizeRemoveEventListenerOptions(options: unknown): boolean {
	if (typeof options === "boolean") {
		return options;
	}

	if (options == null) {
		return false;
	}

	return Boolean((Object(options) as { capture?: boolean }).capture);
}

function isAbortSignalLike(value: unknown): value is AbortSignal {
	return (
		typeof value === "object" &&
		value !== null &&
		"aborted" in value &&
		typeof (value as AbortSignal).addEventListener === "function" &&
		typeof (value as AbortSignal).removeEventListener === "function"
	);
}

class PatchedEvent {
	static readonly NONE = 0;
	static readonly CAPTURING_PHASE = 1;
	static readonly AT_TARGET = 2;
	static readonly BUBBLING_PHASE = 3;

	readonly type: string;
	readonly bubbles: boolean;
	readonly cancelable: boolean;
	readonly composed: boolean;
	detail: unknown = null;
	defaultPrevented = false;
	target: EventTarget | null = null;
	currentTarget: EventTarget | null = null;
	eventPhase = 0;
	returnValue = true;
	cancelBubble = false;
	timeStamp = Date.now();
	isTrusted = false;
	srcElement: EventTarget | null = null;
	private inPassiveListener = false;
	private propagationStopped = false;
	private immediatePropagationStopped = false;

	constructor(type: string, init?: EventInit | null) {
		if (arguments.length === 0) {
			throw new TypeError("The event type must be provided");
		}

		const normalizedInit = init == null ? {} : Object(init);

		this.type = String(type);
		this.bubbles = Boolean((normalizedInit as EventInit).bubbles);
		this.cancelable = Boolean((normalizedInit as EventInit).cancelable);
		this.composed = Boolean((normalizedInit as EventInit).composed);
	}

	get [Symbol.toStringTag](): string {
		return "Event";
	}

	preventDefault(): void {
		if (this.cancelable && !this.inPassiveListener) {
			this.defaultPrevented = true;
			this.returnValue = false;
		}
	}

	stopPropagation(): void {
		this.propagationStopped = true;
		this.cancelBubble = true;
	}

	stopImmediatePropagation(): void {
		this.propagationStopped = true;
		this.immediatePropagationStopped = true;
		this.cancelBubble = true;
	}

	composedPath(): EventTarget[] {
		return this.target ? [this.target] : [];
	}

	_setPassive(value: boolean): void {
		this.inPassiveListener = value;
	}

	_isPropagationStopped(): boolean {
		return this.propagationStopped;
	}

	_isImmediatePropagationStopped(): boolean {
		return this.immediatePropagationStopped;
	}
}

class PatchedCustomEvent extends PatchedEvent {
	constructor(type: string, init?: CustomEventInit<unknown> | null) {
		super(type, init);
		const normalizedInit = init == null ? null : Object(init);
		this.detail =
			normalizedInit && "detail" in normalizedInit
				? (normalizedInit as CustomEventInit<unknown>).detail
				: null;
	}

	get [Symbol.toStringTag](): string {
		return "CustomEvent";
	}
}

class PatchedEventTarget {
	private readonly listeners = new Map<string, ListenerRecord[]>();

	addEventListener(
		type: string,
		listener: EventListenerLike | null,
		options?: boolean | AddEventListenerOptions,
	): undefined {
		const normalized = normalizeAddEventListenerOptions(options);

		if (normalized.signal !== undefined && !isAbortSignalLike(normalized.signal)) {
			throw new TypeError(
				'The "signal" option must be an instance of AbortSignal.',
			);
		}

		if (listener == null) {
			return undefined;
		}

		if (
			typeof listener !== "function" &&
			(typeof listener !== "object" || listener === null)
		) {
			return undefined;
		}

		if (normalized.signal?.aborted) {
			return undefined;
		}

		const records = this.listeners.get(type) ?? [];
		const existing = records.find(
			(record) =>
				record.listener === listener && record.capture === normalized.capture,
		);
		if (existing) {
			return undefined;
		}

		const record: ListenerRecord = {
			listener,
			capture: normalized.capture,
			once: normalized.once,
			passive: normalized.passive,
			kind: typeof listener === "function" ? "function" : "object",
			signal: normalized.signal,
		};

		if (normalized.signal) {
			record.abortListener = () => {
				this.removeEventListener(type, listener, normalized.capture);
			};
			normalized.signal.addEventListener("abort", record.abortListener, {
				once: true,
			});
		}

		records.push(record);
		this.listeners.set(type, records);
		return undefined;
	}

	removeEventListener(
		type: string,
		listener: EventListenerLike | null,
		options?: boolean | EventListenerOptions,
	): void {
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

	dispatchEvent(event: Event): boolean {
		if (
			typeof event !== "object" ||
			event === null ||
			typeof (event as Event).type !== "string"
		) {
			throw new TypeError("Argument 1 must be an Event");
		}

		const patchedEvent = event as unknown as PatchedEvent;
		const records = (this.listeners.get(patchedEvent.type) ?? []).slice();

		patchedEvent.target = this as unknown as EventTarget;
		patchedEvent.currentTarget = this as unknown as EventTarget;
		patchedEvent.eventPhase = 2;

		for (const record of records) {
			const active = this.listeners
				.get(patchedEvent.type)
				?.includes(record);
			if (!active) {
				continue;
			}

			if (record.once) {
				this.removeEventListener(patchedEvent.type, record.listener, record.capture);
			}

			patchedEvent._setPassive(record.passive);

			if (record.kind === "function") {
				(record.listener as (event: PatchedEvent) => void).call(this, patchedEvent);
			} else {
				const handleEvent = (record.listener as { handleEvent?: (event: PatchedEvent) => void }).handleEvent;
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
}

const TextEncoder = PatchedTextEncoder as unknown as typeof globalThis.TextEncoder;
const TextDecoder = PatchedTextDecoder as unknown as typeof globalThis.TextDecoder;
const Event = PatchedEvent as unknown as typeof globalThis.Event;
const CustomEvent = PatchedCustomEvent as unknown as typeof globalThis.CustomEvent;
const EventTarget = PatchedEventTarget as unknown as typeof globalThis.EventTarget;

// Install on globalThis so other modules can use them during load.
defineGlobal("TextEncoder", TextEncoder);
defineGlobal("TextDecoder", TextDecoder);
defineGlobal("Event", Event);
defineGlobal("CustomEvent", CustomEvent);
defineGlobal("EventTarget", EventTarget);

export { TextEncoder, TextDecoder, Event, CustomEvent, EventTarget };
