export const LENGTH_PREFIX_BYTES = 4;

export type ByteArray = Uint8Array<ArrayBufferLike>;

export interface LengthPrefixedPayload {
	payload: ByteArray;
	remaining: ByteArray;
}

export function encodeLengthPrefixedPayload(payload: Uint8Array): Uint8Array {
	const encoded = new Uint8Array(LENGTH_PREFIX_BYTES + payload.length);
	const view = new DataView(encoded.buffer, encoded.byteOffset, encoded.byteLength);
	view.setUint32(0, payload.length, false);
	encoded.set(payload, LENGTH_PREFIX_BYTES);
	return encoded;
}

export function tryDecodeLengthPrefixedPayload(
	buffer: ByteArray,
): LengthPrefixedPayload | null {
	if (buffer.length < LENGTH_PREFIX_BYTES) {
		return null;
	}

	const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
	const declaredLength = view.getUint32(0, false);
	const frameEnd = LENGTH_PREFIX_BYTES + declaredLength;
	if (buffer.length < frameEnd) {
		return null;
	}

	return {
		payload: buffer.subarray(LENGTH_PREFIX_BYTES, frameEnd),
		remaining: buffer.subarray(frameEnd),
	};
}
