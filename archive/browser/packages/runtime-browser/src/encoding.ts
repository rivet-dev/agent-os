export function toUint8Array(value: unknown): Uint8Array {
	if (value instanceof Uint8Array) {
		return value;
	}
	if (ArrayBuffer.isView(value)) {
		return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
	}
	if (value instanceof ArrayBuffer) {
		return new Uint8Array(value);
	}
	return new TextEncoder().encode(String(value));
}

export function bytesToBase64(value: Uint8Array): string {
	const bufferCtor = (
		globalThis as {
			Buffer?: {
				from(value: Uint8Array): { toString(encoding: "base64"): string };
			};
		}
	).Buffer;
	if (bufferCtor) {
		return bufferCtor.from(value).toString("base64");
	}

	let binary = "";
	for (let offset = 0; offset < value.byteLength; offset += 0x8000) {
		const chunk = value.subarray(offset, offset + 0x8000);
		binary += String.fromCharCode(...chunk);
	}
	return btoa(binary);
}

export function base64ToBytes(value: string): Uint8Array {
	const bufferCtor = (
		globalThis as {
			Buffer?: {
				from(value: string, encoding: "base64"): Uint8Array;
			};
		}
	).Buffer;
	if (bufferCtor) {
		return new Uint8Array(bufferCtor.from(value, "base64"));
	}

	const binary = atob(value);
	const out = new Uint8Array(binary.length);
	for (let offset = 0; offset < binary.length; offset += 1) {
		out[offset] = binary.charCodeAt(offset);
	}
	return out;
}

export function guestEncodingBootstrapCode(): string {
	return `
		if (!globalThis.__agentOSEncoding) {
			const encoder = new TextEncoder();
			const bytesToBase64 = (bytes) => {
				let binary = "";
				for (let offset = 0; offset < bytes.byteLength; offset += 0x8000) {
					const chunk = bytes.subarray(offset, offset + 0x8000);
					binary += String.fromCharCode(...chunk);
				}
				return btoa(binary);
			};
			const base64ToBytes = (value) => {
				const binary = atob(value);
				const out = new Uint8Array(binary.length);
				for (let index = 0; index < binary.length; index += 1) {
					out[index] = binary.charCodeAt(index);
				}
				return out;
			};
			const toBytes = (value, encoding) => {
				if (value == null) return new Uint8Array(0);
				if (typeof value === "string") {
					if (encoding === "hex") {
						const out = new Uint8Array(Math.floor(value.length / 2));
						for (let index = 0; index < out.length; index += 1) {
							out[index] = parseInt(value.slice(index * 2, index * 2 + 2), 16);
						}
						return out;
					}
					if (encoding === "base64") return base64ToBytes(value);
					return encoder.encode(value);
				}
				if (ArrayBuffer.isView(value)) {
					return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
				}
				if (value instanceof ArrayBuffer) return new Uint8Array(value);
				if (Array.isArray(value)) return new Uint8Array(value);
				if (value && value.type === "Buffer" && Array.isArray(value.data)) return new Uint8Array(value.data);
				if (value && typeof value === "object" && value.type === "secret" && typeof value.export === "function") {
					return toBytes(value.export());
				}
				return encoder.encode(String(value));
			};
			Object.defineProperty(globalThis, "__agentOSEncoding", {
				configurable: true,
				value: {
					base64ToBytes,
					bytesToBase64,
					decodeBytesPayload(value) {
						if (typeof value === "string") return encoder.encode(value);
						if (!value || value.__agentOSType !== "bytes" || typeof value.base64 !== "string") {
							return new Uint8Array(0);
						}
						return base64ToBytes(value.base64);
					},
					encodeBytesPayload(value) {
						if (value == null) return null;
						return { __agentOSType: "bytes", base64: bytesToBase64(toBytes(value)) };
					},
					toBytes,
				},
			});
		}
	`;
}
