"use strict";

import streamDefault, * as streamNs from "agent-os-stream-stdlib";

const baseStreamModule = streamNs.default ?? streamDefault ?? {};
const baseFinished = streamNs.finished ?? baseStreamModule.finished;

const isWebReadableStream = (stream) =>
	Boolean(stream) &&
	typeof stream.getReader === "function" &&
	typeof stream.cancel === "function";

const isWebWritableStream = (stream) =>
	Boolean(stream) &&
	typeof stream.getWriter === "function" &&
	typeof stream.abort === "function";

const normalizeStreamError = (value) => {
	if (value instanceof Error) {
		return value;
	}
	if (value == null) {
		return new Error("stream errored");
	}
	return new Error(String(value));
};

export const finished = (stream, options, callback) => {
	let normalizedOptions = options;
	let normalizedCallback = callback;
	if (typeof normalizedOptions === "function") {
		normalizedCallback = normalizedOptions;
		normalizedOptions = {};
	}
	if (
		!isWebReadableStream(stream) &&
		!isWebWritableStream(stream) &&
		typeof baseFinished === "function"
	) {
		return baseFinished(stream, normalizedOptions, normalizedCallback);
	}

	const done =
		typeof normalizedCallback === "function" ? normalizedCallback : () => {};
	const readableEnabled = normalizedOptions?.readable !== false;
	const writableEnabled = normalizedOptions?.writable !== false;
	let cancelled = false;
	let timer = null;

	const cleanup = () => {
		cancelled = true;
		if (timer !== null) {
			clearTimeout(timer);
			timer = null;
		}
	};

	const complete = (error = undefined) => {
		if (cancelled) {
			return;
		}
		cleanup();
		queueMicrotask(() => done(error));
	};

	const poll = () => {
		if (cancelled) {
			return;
		}
		const state = stream?._state;
		if (state === "errored") {
			complete(normalizeStreamError(stream?._storedError));
			return;
		}
		if (
			state === "closed" ||
			(isWebReadableStream(stream) && !readableEnabled) ||
			(isWebWritableStream(stream) && !writableEnabled)
		) {
			complete();
			return;
		}
		timer = setTimeout(poll, 0);
	};

	poll();
	return cleanup;
};

export const isReadable = (stream) => {
	if (isWebReadableStream(stream)) {
		return stream._state === "readable";
	}
	return Boolean(stream) && stream.readable !== false && stream.destroyed !== true;
};

export const isErrored = (stream) => {
	if (isWebReadableStream(stream) || isWebWritableStream(stream)) {
		return stream?._state === "errored";
	}
	return stream?.errored != null;
};

export const isDisturbed = (stream) => {
	return Boolean(
		stream?.locked ||
			stream?.disturbed === true ||
			stream?._disturbed === true ||
			stream?.readableDidRead === true,
	);
};

export * from "agent-os-stream-stdlib";

export default {
	...baseStreamModule,
	finished,
	isReadable,
	isErrored,
	isDisturbed,
};
