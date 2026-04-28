"use strict";

function getDnsPromisesModule() {
	const mod = globalThis._dnsModule?.promises;
	if (!mod) {
		throw new Error("node:dns/promises bridge module is not available");
	}
	return mod;
}

const exported = {};
for (const key of [
	"Resolver",
	"lookup",
	"resolve",
	"resolve4",
	"resolve6",
	"resolveAny",
	"resolveMx",
	"resolveTxt",
	"resolveSrv",
	"resolveCname",
	"resolvePtr",
	"resolveNs",
	"resolveSoa",
	"resolveNaptr",
	"resolveCaa",
]) {
	Object.defineProperty(exported, key, {
		enumerable: true,
		get() {
			return getDnsPromisesModule()[key];
		},
	});
}

module.exports = exported;
