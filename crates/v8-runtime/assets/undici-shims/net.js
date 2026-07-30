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
	"BlockList",
	"Socket",
	"SocketAddress",
	"Server",
	"Stream",
	"connect",
	"createConnection",
	"createServer",
	"getDefaultAutoSelectFamily",
	"getDefaultAutoSelectFamilyAttemptTimeout",
	"isIP",
	"isIPv4",
	"isIPv6",
	"setDefaultAutoSelectFamily",
	"setDefaultAutoSelectFamilyAttemptTimeout",
]) {
	Object.defineProperty(exported, key, {
		enumerable: true,
		get() {
			return getNetModule()[key];
		},
	});
}

module.exports = exported;
