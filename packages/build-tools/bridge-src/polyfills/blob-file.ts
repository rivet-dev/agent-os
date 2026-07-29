import { Blob as UpstreamBlob } from "fetch-blob";
import { File as UpstreamFile } from "fetch-blob/file.js";

const MAX_BLOB_PARTS = 1024;
const MAX_BLOB_BYTES = 64 * 1024 * 1024;

function blobPartSize(part) {
	if (part instanceof UpstreamBlob) return part.size;
	if (ArrayBuffer.isView(part)) return part.byteLength;
	if (part instanceof ArrayBuffer) return part.byteLength;
	return new TextEncoder().encode(`${part}`).byteLength;
}

function normalizeBlobParts(parts) {
	if (parts === void 0) return [];
	if (parts == null || typeof parts[Symbol.iterator] !== "function") {
		throw new TypeError("Blob parts must be an iterable");
	}
	const normalized = [];
	let totalBytes = 0;
	for (const part of parts) {
		if (normalized.length >= MAX_BLOB_PARTS) {
			const error = new RangeError(
				`Blob part limit ${MAX_BLOB_PARTS} exceeded; this runtime limit cannot be raised by guest code`,
			);
			error.code = "ERR_BLOB_PARTS_LIMIT";
			throw error;
		}
		totalBytes += blobPartSize(part);
		if (!Number.isSafeInteger(totalBytes) || totalBytes > MAX_BLOB_BYTES) {
			const error = new RangeError(
				`Blob byte limit ${MAX_BLOB_BYTES} exceeded; this runtime limit cannot be raised by guest code`,
			);
			error.code = "ERR_BLOB_SIZE_LIMIT";
			throw error;
		}
		normalized.push(part);
	}
	return normalized;
}

function normalizeBlobOptions(options) {
	if (options == null || options.type === void 0) {
		return options;
	}
	const type = String(options.type);
	return {
		...Object(options),
		type: /^[\x20-\x7e]*$/.test(type) ? type.toLowerCase() : "",
	};
}

class Blob extends UpstreamBlob {
	constructor(parts, options) {
		super(normalizeBlobParts(parts), normalizeBlobOptions(options));
	}

	slice(start, end, type) {
		const sliced = super.slice(start, end, type);
		return new Blob([sliced], { type: sliced.type });
	}
}

Object.setPrototypeOf(UpstreamFile.prototype, Blob.prototype);

class File extends UpstreamFile {
	constructor(parts, name, options) {
		super(normalizeBlobParts(parts), name, normalizeBlobOptions(options));
	}
}

export {
	Blob,
	File,
	MAX_BLOB_BYTES,
	MAX_BLOB_PARTS,
	normalizeBlobOptions,
	normalizeBlobParts,
};
