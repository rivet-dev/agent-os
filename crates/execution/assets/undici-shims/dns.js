"use strict";

function getDnsModule() {
	const mod = globalThis._dnsModule;
	if (!mod) {
		throw new Error("node:dns bridge module is not available");
	}
	return mod;
}

const exported = {};
for (const key of ["lookup", "resolve", "resolve4", "resolve6", "promises"]) {
	Object.defineProperty(exported, key, {
		enumerable: true,
		get() {
			return getDnsModule()[key];
		},
	});
}

module.exports = exported;
