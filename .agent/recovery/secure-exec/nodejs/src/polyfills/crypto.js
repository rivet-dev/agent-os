import cryptoBrowserify from "__secure_exec_crypto_browserify__";

function createInvalidArgTypeError(name, expected, actual) {
	return new TypeError(
		`The "${name}" argument must be ${expected}. Received type ${typeof actual}`,
	);
}

const cryptoModule = cryptoBrowserify;

if (typeof globalThis.crypto === "object" && globalThis.crypto !== null) {
	if (
		typeof cryptoModule.getRandomValues !== "function" &&
		typeof globalThis.crypto.getRandomValues === "function"
	) {
		cryptoModule.getRandomValues = function getRandomValues(array) {
			return globalThis.crypto.getRandomValues(array);
		};
	}

	if (
		typeof cryptoModule.randomUUID !== "function" &&
		typeof globalThis.crypto.randomUUID === "function"
	) {
		cryptoModule.randomUUID = function randomUUID(options) {
			if (options !== undefined) {
				if (options === null || typeof options !== "object") {
					throw createInvalidArgTypeError("options", "of type object", options);
				}
				if (
					Object.prototype.hasOwnProperty.call(options, "disableEntropyCache") &&
					typeof options.disableEntropyCache !== "boolean"
				) {
					throw createInvalidArgTypeError(
						"options.disableEntropyCache",
						"of type boolean",
						options.disableEntropyCache,
					);
				}
			}
			return globalThis.crypto.randomUUID();
		};
	}

	if (typeof cryptoModule.webcrypto === "undefined") {
		cryptoModule.webcrypto = globalThis.crypto;
	}
}

export default cryptoModule;
export const randomUUID = cryptoModule.randomUUID;
export const webcrypto = cryptoModule.webcrypto;
