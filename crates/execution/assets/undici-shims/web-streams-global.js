"use strict";

import {
	ReadableStream as WebReadableStream,
	WritableStream as WebWritableStream,
	TransformStream as WebTransformStream,
} from "web-streams-polyfill/ponyfill/es2018";

if (typeof globalThis.ReadableStream === "undefined") {
	globalThis.ReadableStream = WebReadableStream;
}
if (typeof globalThis.WritableStream === "undefined") {
	globalThis.WritableStream = WebWritableStream;
}
if (typeof globalThis.TransformStream === "undefined") {
	globalThis.TransformStream = WebTransformStream;
}

export {
	WebReadableStream,
	WebWritableStream,
	WebTransformStream,
};
