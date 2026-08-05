"use strict";

const crypto = require("agentos-legacy-crypto-polyfill");

const RANDOM_INT_LIMIT = 2 ** 48;

function randomInt(min, max, callback) {
	if (typeof max === "function") {
		callback = max;
		max = min;
		min = 0;
	} else if (max === undefined) {
		max = min;
		min = 0;
	}
	if (
		!Number.isSafeInteger(min) ||
		!Number.isSafeInteger(max) ||
		min < 0 ||
		max <= min ||
		max - min > RANDOM_INT_LIMIT
	) {
		throw new RangeError("The value of max - min is out of range");
	}
	const range = max - min;
	const unbiasedLimit = RANDOM_INT_LIMIT - (RANDOM_INT_LIMIT % range);
	let value;
	do {
		value = crypto.randomBytes(6).readUIntBE(0, 6);
	} while (value >= unbiasedLimit);
	const result = min + (value % range);
	if (typeof callback === "function") {
		process.nextTick(callback, null, result);
		return;
	}
	return result;
}

crypto.randomInt = randomInt;
module.exports = crypto;
