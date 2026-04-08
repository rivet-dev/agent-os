"use strict";

const performance =
	globalThis.performance ??
	({
		now() {
			return Date.now();
		},
		timeOrigin: Date.now(),
	});

module.exports = { performance };
