"use strict";

function isArrayBuffer(value) {
	return value instanceof ArrayBuffer;
}

function isArrayBufferView(value) {
	return ArrayBuffer.isView(value);
}

function isUint8Array(value) {
	return value instanceof Uint8Array;
}

function isProxy(_value) {
	return false;
}

module.exports = {
	isArrayBuffer,
	isArrayBufferView,
	isProxy,
	isUint8Array,
};
