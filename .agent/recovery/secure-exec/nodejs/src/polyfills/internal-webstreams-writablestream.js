import { getWebStreamsState } from "./webstreams-runtime.js";

const state = getWebStreamsState();

export const WritableStream = state.WritableStream;
export const isWritableStream = state.isWritableStream;
