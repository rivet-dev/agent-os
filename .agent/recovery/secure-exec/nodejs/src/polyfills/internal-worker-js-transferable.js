import { getJsTransferState } from "./js-transferable.js";

const state = getJsTransferState();

export const kClone = state.kClone;
export const kDeserialize = state.kDeserialize;
export const kTransfer = state.kTransfer;
export const kTransferList = state.kTransferList;
export const markTransferMode = state.markTransferMode;
