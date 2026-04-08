import { getWebStreamsState } from "./webstreams-runtime.js";

const state = getWebStreamsState();

export const ReadableStream = state.ReadableStream;
export const ReadableStreamDefaultReader = state.ReadableStreamDefaultReader;
export const ReadableStreamBYOBReader = state.ReadableStreamBYOBReader;
export const ReadableStreamBYOBRequest = state.ReadableStreamBYOBRequest;
export const ReadableByteStreamController = state.ReadableByteStreamController;
export const ReadableStreamDefaultController = state.ReadableStreamDefaultController;
export const TransformStream = state.TransformStream;
export const TransformStreamDefaultController = state.TransformStreamDefaultController;
export const WritableStream = state.WritableStream;
export const WritableStreamDefaultWriter = state.WritableStreamDefaultWriter;
export const WritableStreamDefaultController = state.WritableStreamDefaultController;
export const ByteLengthQueuingStrategy = state.ByteLengthQueuingStrategy;
export const CountQueuingStrategy = state.CountQueuingStrategy;
export const TextEncoderStream = state.TextEncoderStream;
export const TextDecoderStream = state.TextDecoderStream;
export const CompressionStream = state.CompressionStream;
export const DecompressionStream = state.DecompressionStream;
