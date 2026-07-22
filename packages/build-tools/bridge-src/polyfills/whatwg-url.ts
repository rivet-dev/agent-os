import {
	URL as UpstreamURL,
	URLSearchParams as UpstreamURLSearchParams,
} from "whatwg-url";

const kBlobUrlStore = /* @__PURE__ */ Symbol.for("agentOs.blobUrlStore");
const kBlobUrlCounter = /* @__PURE__ */ Symbol.for("agentOs.blobUrlCounter");
const MAX_BLOB_URLS = 1024;

function createNodeTypeError(message, code) {
	const error = new TypeError(message);
	error.code = code;
	return error;
}

function createMissingArgsError(message) {
	return createNodeTypeError(message, "ERR_MISSING_ARGS");
}

function getBlobUrlStore() {
	const globalRecord = globalThis;
	const existing = globalRecord[kBlobUrlStore];
	if (existing instanceof Map) {
		return existing;
	}
	const store = /* @__PURE__ */ new Map();
	globalRecord[kBlobUrlStore] = store;
	return store;
}

function nextBlobUrlId() {
	const globalRecord = globalThis;
	const nextId =
		typeof globalRecord[kBlobUrlCounter] === "number"
			? globalRecord[kBlobUrlCounter]
			: 1;
	globalRecord[kBlobUrlCounter] = nextId + 1;
	return nextId;
}

function resolveObjectURL(url) {
	return getBlobUrlStore().get(typeof url === "string" ? url : `${url}`);
}

const URL2 = UpstreamURL;
const URLSearchParams = UpstreamURLSearchParams;

Object.defineProperties(URL2, {
	createObjectURL: {
		value(obj) {
			const Blob = globalThis.Blob;
			if (typeof Blob !== "function" || !(obj instanceof Blob)) {
				throw createNodeTypeError(
					'The "obj" argument must be an instance of Blob. Received ' +
						(obj === null ? "null" : typeof obj),
					"ERR_INVALID_ARG_TYPE",
				);
			}
			const store = getBlobUrlStore();
			if (store.size >= MAX_BLOB_URLS) {
				const error = new Error(
					`Blob URL limit of ${MAX_BLOB_URLS} reached. Revoke unused object URLs or increase MAX_BLOB_URLS in the runtime build.`,
				);
				error.code = "ERR_BLOB_URL_STORE_LIMIT";
				throw error;
			}
			const id = `blob:nodedata:${nextBlobUrlId()}`;
			store.set(id, obj);
			return id;
		},
		writable: true,
		configurable: true,
		enumerable: true,
	},
	revokeObjectURL: {
		value(...args) {
			if (args.length < 1) {
				throw createMissingArgsError('The "url" argument must be specified');
			}
			const [url] = args;
			getBlobUrlStore().delete(typeof url === "string" ? url : `${url}`);
		},
		writable: true,
		configurable: true,
		enumerable: true,
	},
});

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
	MAX_BLOB_URLS,
	nextBlobUrlId,
	resolveObjectURL,
	URL2,
	URLSearchParams,
};
