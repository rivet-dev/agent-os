"use strict";

function getNetModule() {
	const mod = globalThis._netModule;
	if (!mod) {
		throw new Error("node:net bridge module is not available");
	}
	return mod;
}

const exported = {};
for (const key of [
	"Socket",
	"Server",
	"connect",
	"createConnection",
	"createServer",
	"isIP",
	"isIPv4",
	"isIPv6",
]) {
	Object.defineProperty(exported, key, {
		enumerable: true,
		get() {
			return getNetModule()[key];
		},
	});
}

module.exports = exported;
