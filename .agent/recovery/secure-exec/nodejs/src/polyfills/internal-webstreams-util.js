import { getWebStreamsState } from "./webstreams-runtime.js";

const state = getWebStreamsState();

export const kState = state.kState;
export const isPromisePending = state.isPromisePending;
export function customInspect(value, inspect) {
	return inspect(value);
}
