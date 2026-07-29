import {
	TextDecoder as UpstreamTextDecoder,
	TextEncoder as UpstreamTextEncoder,
	TextEncoderStream as UpstreamTextEncoderStream,
} from "@exodus/bytes/encoding-lite.js";

function withCode(error, code) {
	error.code = code;
	return error;
}

function createEncodingNotSupportedError(label) {
	return withCode(
		new RangeError(`The "${label}" encoding is not supported`),
		"ERR_ENCODING_NOT_SUPPORTED",
	);
}

function createEncodingInvalidDataError(encoding) {
	return withCode(
		new TypeError(`The encoded data was not valid for encoding ${encoding}`),
		"ERR_ENCODING_INVALID_ENCODED_DATA",
	);
}

function createInvalidDecodeInputError() {
	return withCode(
		new TypeError(
			'The "input" argument must be an instance of ArrayBuffer, SharedArrayBuffer, or ArrayBufferView.',
		),
		"ERR_INVALID_ARG_TYPE",
	);
}

function trimAsciiWhitespace(value) {
	return value.replace(/^[\t\n\f\r ]+|[\t\n\f\r ]+$/g, "");
}

function normalizeEncodingLabel(label) {
	const normalized = trimAsciiWhitespace(
		label === void 0 ? "utf-8" : `${label}`,
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
	if (
		typeof SharedArrayBuffer !== "undefined" &&
		input instanceof SharedArrayBuffer
	) {
		return new Uint8Array(input);
	}
	throw createInvalidDecodeInputError();
}

class PatchedTextEncoder extends UpstreamTextEncoder {}

class PatchedTextDecoder extends UpstreamTextDecoder {
	constructor(label, options) {
		super(
			normalizeEncodingLabel(label),
			options == null ? {} : Object(options),
		);
	}

	decode(input, options) {
		const source = toUint8Array(input);
		const decodeOptions = options == null ? {} : Object(options);
		const stream = Boolean(decodeOptions.stream);
		try {
			return super.decode(source, { stream });
		} catch (error) {
			if (this.fatal && error instanceof TypeError) {
				throw createEncodingInvalidDataError(this.encoding);
			}
			throw error;
		}
	}
}

class PatchedTextDecoderStream {
	constructor(label, options) {
		const decoder = new PatchedTextDecoder(label, options);
		const transform = new TransformStream({
			transform(chunk, controller) {
				const output = decoder.decode(chunk, { stream: true });
				if (output) controller.enqueue(output);
			},
			flush(controller) {
				const output = decoder.decode();
				if (output) controller.enqueue(output);
			},
		});
		this.readable = transform.readable;
		this.writable = transform.writable;
		this.encoding = decoder.encoding;
		this.fatal = decoder.fatal;
		this.ignoreBOM = decoder.ignoreBOM;
	}
}

Object.defineProperty(PatchedTextEncoder, "name", {
	configurable: true,
	value: "TextEncoder",
});
Object.defineProperty(PatchedTextDecoder, "name", {
	configurable: true,
	value: "TextDecoder",
});

var TextEncoder2 = PatchedTextEncoder;
var TextDecoder = PatchedTextDecoder;
var TextEncoderStream = UpstreamTextEncoderStream;
var TextDecoderStream = PatchedTextDecoderStream;

export {
	createEncodingInvalidDataError,
	createEncodingNotSupportedError,
	createInvalidDecodeInputError,
	normalizeEncodingLabel,
	PatchedTextDecoder,
	PatchedTextEncoder,
	TextDecoder,
	TextDecoderStream,
	TextEncoder2,
	TextEncoderStream,
	toUint8Array,
	trimAsciiWhitespace,
	withCode,
};
