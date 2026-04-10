"use strict";

import {
	ReadableStream as WebReadableStream,
	WritableStream as WebWritableStream,
	TransformStream as WebTransformStream,
} from "web-streams-polyfill/ponyfill/es2018";

class FallbackTextEncoderStream {
	constructor() {
		const encoder = new globalThis.TextEncoder();
		const stream = new WebTransformStream({
			transform(chunk, controller) {
				controller.enqueue(encoder.encode(chunk));
			},
		});

		this.encoding = "utf-8";
		this.readable = stream.readable;
		this.writable = stream.writable;
	}
}

class FallbackTextDecoderStream {
	constructor(label = "utf-8", options = undefined) {
		const decoder = new globalThis.TextDecoder(label, options);
		const stream = new WebTransformStream({
			transform(chunk, controller) {
				const text = decoder.decode(chunk, { stream: true });
				if (text.length > 0) {
					controller.enqueue(text);
				}
			},
			flush(controller) {
				const text = decoder.decode();
				if (text.length > 0) {
					controller.enqueue(text);
				}
			},
		});

		this.encoding = decoder.encoding;
		this.fatal = decoder.fatal;
		this.ignoreBOM = decoder.ignoreBOM;
		this.readable = stream.readable;
		this.writable = stream.writable;
	}
}

const WebTextEncoderStream =
	typeof globalThis.TextEncoderStream === "function"
		? globalThis.TextEncoderStream
		: FallbackTextEncoderStream;
const WebTextDecoderStream =
	typeof globalThis.TextDecoderStream === "function"
		? globalThis.TextDecoderStream
		: FallbackTextDecoderStream;

if (typeof globalThis.ReadableStream === "undefined") {
	globalThis.ReadableStream = WebReadableStream;
}
if (typeof globalThis.WritableStream === "undefined") {
	globalThis.WritableStream = WebWritableStream;
}
if (typeof globalThis.TransformStream === "undefined") {
	globalThis.TransformStream = WebTransformStream;
}
if (typeof globalThis.TextEncoderStream === "undefined") {
	globalThis.TextEncoderStream = WebTextEncoderStream;
}
if (typeof globalThis.TextDecoderStream === "undefined") {
	globalThis.TextDecoderStream = WebTextDecoderStream;
}

export {
	WebReadableStream,
	WebWritableStream,
	WebTransformStream,
	WebTextEncoderStream,
	WebTextDecoderStream,
};
