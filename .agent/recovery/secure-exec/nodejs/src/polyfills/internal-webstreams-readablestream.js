import { getWebStreamsState } from "./webstreams-runtime.js";

const state = getWebStreamsState();

export const ReadableStream = state.ReadableStream;
export const isReadableStream = state.isReadableStream;
export const readableStreamPipeTo = state.readableStreamPipeTo;
export const readableStreamTee = state.readableStreamTee;
export const readableByteStreamControllerConvertPullIntoDescriptor =
	state.readableByteStreamControllerConvertPullIntoDescriptor;
export const readableStreamDefaultControllerEnqueue =
	state.readableStreamDefaultControllerEnqueue;
export const readableByteStreamControllerEnqueue = state.readableByteStreamControllerEnqueue;
export const readableStreamDefaultControllerCanCloseOrEnqueue =
	state.readableStreamDefaultControllerCanCloseOrEnqueue;
export const readableByteStreamControllerClose = state.readableByteStreamControllerClose;
export const readableByteStreamControllerRespond = state.readableByteStreamControllerRespond;
export const readableStreamReaderGenericRelease = state.readableStreamReaderGenericRelease;
