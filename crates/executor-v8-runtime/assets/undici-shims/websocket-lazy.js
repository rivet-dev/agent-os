"use strict";

let cached;

module.exports = function loadWebSocketModule() {
	if (cached === undefined) {
		cached = require("ws");
	}
	return cached;
};
