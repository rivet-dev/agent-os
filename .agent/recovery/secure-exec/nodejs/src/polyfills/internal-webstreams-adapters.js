import { getWebStreamsState } from "./webstreams-runtime.js";

const state = getWebStreamsState();

export const newReadableStreamFromStreamReadable = state.newReadableStreamFromStreamReadable;
export const newStreamReadableFromReadableStream = state.newStreamReadableFromReadableStream;
export const newWritableStreamFromStreamWritable = state.newWritableStreamFromStreamWritable;
export const newStreamWritableFromWritableStream = state.newStreamWritableFromWritableStream;
export const newReadableWritablePairFromDuplex = state.newReadableWritablePairFromDuplex;
export const newStreamDuplexFromReadableWritablePair = state.newStreamDuplexFromReadableWritablePair;
export const newWritableStreamFromStreamBase = state.newWritableStreamFromStreamBase;
export const newReadableStreamFromStreamBase = state.newReadableStreamFromStreamBase;
