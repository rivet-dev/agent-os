// Standard base64 codec over byte arrays, shared by the converged bridge
// translation layers. Self-contained (no `atob`/`btoa`) so it behaves
// identically in a Worker, the main thread, and Node/vitest, and is binary-safe
// (atob/btoa are latin1-only).

const BASE64_ALPHABET =
	"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

export function encodeBase64(bytes: Uint8Array): string {
	let output = "";
	for (let index = 0; index < bytes.length; index += 3) {
		const byte0 = bytes[index];
		const byte1 = bytes[index + 1];
		const byte2 = bytes[index + 2];
		const triple = (byte0 << 16) | ((byte1 ?? 0) << 8) | (byte2 ?? 0);
		output += BASE64_ALPHABET[(triple >> 18) & 0x3f];
		output += BASE64_ALPHABET[(triple >> 12) & 0x3f];
		output +=
			index + 1 < bytes.length ? BASE64_ALPHABET[(triple >> 6) & 0x3f] : "=";
		output +=
			index + 2 < bytes.length ? BASE64_ALPHABET[triple & 0x3f] : "=";
	}
	return output;
}

export function decodeBase64(input: string): Uint8Array {
	const clean = input.replace(/[^A-Za-z0-9+/]/g, "");
	const length = Math.floor((clean.length * 3) / 4);
	const bytes = new Uint8Array(length);
	let outIndex = 0;
	for (let index = 0; index < clean.length; index += 4) {
		const enc0 = BASE64_ALPHABET.indexOf(clean[index]);
		const enc1 = BASE64_ALPHABET.indexOf(clean[index + 1]);
		const enc2 = BASE64_ALPHABET.indexOf(clean[index + 2]);
		const enc3 = BASE64_ALPHABET.indexOf(clean[index + 3]);
		const triple =
			(enc0 << 18) | (enc1 << 12) | ((enc2 & 0x3f) << 6) | (enc3 & 0x3f);
		if (outIndex < length) bytes[outIndex++] = (triple >> 16) & 0xff;
		if (enc2 !== -1 && outIndex < length) bytes[outIndex++] = (triple >> 8) & 0xff;
		if (enc3 !== -1 && outIndex < length) bytes[outIndex++] = triple & 0xff;
	}
	return bytes;
}
