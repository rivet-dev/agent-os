const kBlobUrlStore = /* @__PURE__ */ Symbol.for("agentOs.blobUrlStore");
const kBlobUrlCounter = /* @__PURE__ */ Symbol.for("agentOs.blobUrlCounter");

function createNodeTypeError(message, code) {
	const error = new TypeError(message);
	error.code = code;
	return error;
}

function createMissingArgsError(message) {
	return createNodeTypeError(message, "ERR_MISSING_ARGS");
}

function getBlobUrlStore() {
	const existing = globalThis[kBlobUrlStore];
	if (existing instanceof Map) {
		return existing;
	}
	const store = /* @__PURE__ */ new Map();
	globalThis[kBlobUrlStore] = store;
	return store;
}

function nextBlobUrlId() {
	const nextId =
		typeof globalThis[kBlobUrlCounter] === "number"
			? globalThis[kBlobUrlCounter]
			: 1;
	globalThis[kBlobUrlCounter] = nextId + 1;
	return nextId;
}

function resolveObjectURL(url) {
	return getBlobUrlStore().get(typeof url === "string" ? url : `${url}`);
}

const URL2 = globalThis.URL;
const URLSearchParams = globalThis.URLSearchParams;

function installWhatwgUrlGlobals(target = globalThis) {
	Object.defineProperty(target, "URL", {
		value: URL2,
		writable: true,
		configurable: true,
		enumerable: false,
	});
	Object.defineProperty(target, "URLSearchParams", {
		value: URLSearchParams,
		writable: true,
		configurable: true,
		enumerable: false,
	});
}

export {
	createMissingArgsError,
	createNodeTypeError,
	getBlobUrlStore,
	installWhatwgUrlGlobals,
	kBlobUrlCounter,
	kBlobUrlStore,
	nextBlobUrlId,
	resolveObjectURL,
	URL2,
	URLSearchParams,
};
