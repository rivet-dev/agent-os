"use strict";

const legacyUrl = require("agentos-legacy-url-polyfill");

Object.defineProperties(legacyUrl, {
	URL: {
		configurable: true,
		enumerable: true,
		get() {
			return globalThis.URL;
		},
	},
	URLSearchParams: {
		configurable: true,
		enumerable: true,
		get() {
			return globalThis.URLSearchParams;
		},
	},
});

module.exports = legacyUrl;
