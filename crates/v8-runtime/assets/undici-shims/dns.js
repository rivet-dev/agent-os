"use strict";

function getDnsModule() {
	const mod = globalThis._dnsModule;
	if (!mod) {
		throw new Error("node:dns bridge module is not available");
	}
	return mod;
}

const exported = {};
for (const key of [
	"ADDRCONFIG",
	"ALL",
	"Resolver",
	"V4MAPPED",
	"constants",
	"getDefaultResultOrder",
	"getServers",
	"lookup",
	"lookupService",
	"promises",
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
	"reverse",
	"setDefaultResultOrder",
	"setServers",
]) {
	Object.defineProperty(exported, key, {
		enumerable: true,
		get() {
			return getDnsModule()[key];
		},
	});
}

module.exports = exported;
