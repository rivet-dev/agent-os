"use strict";

const WebReadableStream = globalThis.ReadableStream;
const WebWritableStream = globalThis.WritableStream;
const WebTransformStream = globalThis.TransformStream;
const WebTextEncoderStream = globalThis.TextEncoderStream;
const WebTextDecoderStream = globalThis.TextDecoderStream;

for (const [name, constructor] of Object.entries({
	ReadableStream: WebReadableStream,
	WritableStream: WebWritableStream,
	TransformStream: WebTransformStream,
	TextEncoderStream: WebTextEncoderStream,
	TextDecoderStream: WebTextDecoderStream,
})) {
	if (typeof constructor !== "function") {
		throw new Error(`${name} was not installed by the web-platform bootstrap`);
	}
}

export {
	WebReadableStream,
	WebWritableStream,
	WebTransformStream,
	WebTextEncoderStream,
	WebTextDecoderStream,
};
