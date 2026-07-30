"use strict";

function getTlsModule() {
	const mod = globalThis._tlsModule;
	if (!mod) {
		throw new Error("node:tls bridge module is not available");
	}
	return mod;
}

const exported = {};
for (const key of [
	"connect",
	"createServer",
	"createSecureContext",
	"TLSSocket",
	"Server",
	"checkServerIdentity",
	"getCiphers",
	"rootCertificates",
]) {
	Object.defineProperty(exported, key, {
		enumerable: true,
		get() {
			return getTlsModule()[key];
		},
	});
}

module.exports = exported;
