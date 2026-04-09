"use strict";

function getZlibModule() {
	const mod = globalThis.__agentOsBuiltinZlibModule;
	if (!mod) {
		throw new Error("node:zlib bridge module is not available");
	}
	return mod;
}

module.exports = new Proxy(
	{},
	{
		get(_target, property) {
			return getZlibModule()[property];
		},
		has(_target, property) {
			return property in getZlibModule();
		},
		ownKeys() {
			return Reflect.ownKeys(getZlibModule());
		},
		getOwnPropertyDescriptor(_target, property) {
			const descriptor = Object.getOwnPropertyDescriptor(getZlibModule(), property);
			if (descriptor) {
				return descriptor;
			}
			return {
				configurable: true,
				enumerable: true,
				value: undefined,
				writable: false,
			};
		},
	},
);
