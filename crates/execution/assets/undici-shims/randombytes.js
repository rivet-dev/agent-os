"use strict";

const MAX_RANDOM_VALUES_BYTES = 65_536;

module.exports = function randomBytes(size, callback) {
	const length = Number(size);
	if (!Number.isInteger(length) || length < 0 || length > 0xffff_ffff) {
		throw new RangeError("requested too many random bytes");
	}
	const bytes = Buffer.allocUnsafe(length);
	const crypto = globalThis.crypto;
	if (!crypto || typeof crypto.getRandomValues !== "function") {
		throw new Error("Secure random number generation is not available");
	}
	for (let offset = 0; offset < length; offset += MAX_RANDOM_VALUES_BYTES) {
		crypto.getRandomValues(
			bytes.subarray(offset, Math.min(length, offset + MAX_RANDOM_VALUES_BYTES)),
		);
	}
	if (typeof callback === "function") {
		process.nextTick(callback, null, bytes);
		return;
	}
	return bytes;
};
