import { getWebStreamsState } from "./webstreams-runtime.js";

const state = getWebStreamsState();

export const TransformStream = state.TransformStream;
export const isTransformStream = state.isTransformStream;
