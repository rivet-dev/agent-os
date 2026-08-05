export function toExactArrayBuffer(value: Uint8Array): ArrayBuffer {
	return value.buffer.slice(
		value.byteOffset,
		value.byteOffset + value.byteLength,
	) as ArrayBuffer;
}

export function toExactUint8Array(value: Uint8Array): Uint8Array {
	return Uint8Array.from(value);
}
